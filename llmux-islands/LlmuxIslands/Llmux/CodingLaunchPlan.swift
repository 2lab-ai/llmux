import Foundation

enum CodingClient: String, CaseIterable, Identifiable {
    case claude, codex
    var id: String { rawValue }
    var title: String { self == .claude ? "Claude Code" : "Codex" }
    var installCommand: String { self == .claude ? "brew install --cask claude-code" : "brew install --cask codex" }
}

enum CodingLaunchError: Error, LocalizedError {
    case remote, missingTool(String), missingProject, terminal
    var errorDescription: String? {
        switch self {
        case .remote: "Project launch is available for a local llmux connection."
        case .missingTool(let name): "Install \(name), then try again."
        case .missingProject: "Choose an existing project folder first."
        case .terminal: "Terminal could not open. Allow Automation access in System Settings, or copy the command."
        }
    }
}

/// One command plan shared by the launch button and clipboard. Only enum values,
/// verified local tools and a directory enter it; no credentials or model prompts.
struct CodingLaunchPlan {
    let command: String
    init(client: CodingClient, project: URL?, isRemote: Bool,
         findTool: (String) -> String? = InstalledTools.find) throws {
        guard !isRemote else { throw CodingLaunchError.remote }
        guard let llmux = findTool("llmux") else { throw CodingLaunchError.missingTool("llmux") }
        guard findTool(client.rawValue) != nil else { throw CodingLaunchError.missingTool(client.title) }
        var directory: ObjCBool = false
        guard let project, project.isFileURL,
              FileManager.default.fileExists(atPath: project.path, isDirectory: &directory), directory.boolValue
        else { throw CodingLaunchError.missingProject }
        let args = [llmux, "run"] + (client == .codex ? ["--codex"] : [])
        command = "cd -- \(Self.quote(project.path)) && exec " + args.map(Self.quote).joined(separator: " ")
    }
    static func quote(_ value: String) -> String {
        "'" + value.replacingOccurrences(of: "'", with: "'\\''") + "'"
    }
    // Command passed as argv; interpolating project text into AppleScript would
    // introduce a second language escape boundary and is deliberately avoided.
    static let terminalScript = """
    on run argv
        tell application "Terminal"
            activate
            do script (item 1 of argv)
        end tell
    end run
    """
    func openTerminal() async throws {
        let command = command
        do {
            _ = try await Task.detached {
                try BoundedLocalProcess.output(executable: "/usr/bin/osascript",
                    arguments: ["-e", Self.terminalScript, command], timeout: 15)
            }.value
        } catch { throw CodingLaunchError.terminal }
    }
}
