import AppKit
import SwiftUI

/// Native executor UI: selecting a folder never starts a process. Terminal is
/// opened only by an explicit button press, and never in offscreen snapshots.
struct StartCodingView: View {
    var isOnline: Bool
    var fixtureProject: URL? = nil
    @State private var client: CodingClient = .claude
    @State private var project: URL?
    @State private var message: String?
    @State private var launching = false

    private var selectedProject: URL? { project ?? fixtureProject }
    private var remote: Bool { SnapshotMode.isActive ? false : ((try? LlmuxClient.current().isRemoteEndpoint()) ?? true) }
    private var missingTool: String? {
        if SnapshotMode.isActive { return nil }
        if InstalledTools.find("llmux") == nil { return "llmux" }
        return InstalledTools.find(client.rawValue) == nil ? client.title : nil
    }
    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Text("Start coding").font(.system(size: 14, weight: .semibold))
                Spacer()
                Text("Your project. Your AI tools.").font(.system(size: 11)).foregroundStyle(.white.opacity(0.5))
            }
            Picker("Coding app", selection: $client) {
                ForEach(CodingClient.allCases) { Text($0.title).tag($0) }
            }
            .pickerStyle(.segmented)
            HStack(spacing: 8) {
                Button(action: chooseProject) {
                    Label(selectedProject?.lastPathComponent ?? "Choose project folder", systemImage: "folder")
                        .lineLimit(1).truncationMode(.middle).frame(maxWidth: .infinity, alignment: .leading)
                }
                .help(selectedProject?.path ?? "Choose the folder you want to work in")
                Button(launching ? "Opening…" : "Open in Terminal", action: launch)
                    .buttonStyle(.borderedProminent).tint(TerminalColors.green)
                    .disabled(!isOnline || selectedProject == nil || missingTool != nil || remote || launching)
            }
            if remote {
                Text("Use llmux run on your computer for remote connections.").font(.system(size: 11)).foregroundStyle(.white.opacity(0.6))
            } else if let missingTool {
                HStack {
                    Text("Install \(missingTool) to continue.").font(.system(size: 11))
                    Spacer()
                    Button("Copy install command") {
                        guard !SnapshotMode.isActive else { return }
                        copy(missingTool == "llmux" ? "brew install 2lab-ai/tap/llmux" : client.installCommand)
                        message = "Install command copied. Run it in Terminal, then reopen Islands."
                    }.font(.system(size: 11))
                }
            } else {
                HStack {
                    Text("Opens your chosen app through llmux.").font(.system(size: 11)).foregroundStyle(.white.opacity(0.5))
                    Spacer()
                    Button("Copy command", action: copyCommand).font(.system(size: 11))
                        .disabled(selectedProject == nil || remote)
                }
            }
            if let message { Text(message).font(.system(size: 11)).foregroundStyle(.white.opacity(0.8)).fixedSize(horizontal: false, vertical: true) }
        }
        .foregroundStyle(.white)
        .padding(14)
        .background(RoundedRectangle(cornerRadius: 14).fill(Color.white.opacity(0.055)))
        .onChange(of: client) { _, _ in message = nil }
    }
    private func chooseProject() {
        guard !SnapshotMode.isActive else { return }
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.prompt = "Choose project"
        panel.message = "Choose the folder Claude Code or Codex should work in."
        if panel.runModal() == .OK { project = panel.url; message = nil }
    }
    private func plan() throws -> CodingLaunchPlan { try CodingLaunchPlan(client: client, project: selectedProject, isRemote: remote) }
    private func copyCommand() {
        guard !SnapshotMode.isActive else { return }
        do { copy(try plan().command); message = "Command copied." }
        catch { message = error.localizedDescription }
    }
    private func launch() {
        guard !SnapshotMode.isActive, isOnline, !launching else { return }
        do {
            let plan = try plan()
            launching = true; message = nil
            Task { @MainActor in
                defer { launching = false }
                do { try await plan.openTerminal(); message = "Opened in Terminal. Continue there." }
                catch { message = error.localizedDescription }
            }
        } catch { message = error.localizedDescription }
    }
    private func copy(_ text: String) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(text, forType: .string)
    }
}
