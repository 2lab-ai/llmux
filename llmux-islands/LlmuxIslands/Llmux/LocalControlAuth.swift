import Foundation
import Darwin

/// Secrets in this file belong only to the native HTTP executor, never UiState.
enum LocalConnectionError: Error, LocalizedError, Equatable {
    case unavailable, invalid, changed, timedOut
    var errorDescription: String? {
        switch self {
        case .unavailable: "Update llmux, then retry the local connection."
        case .invalid: "This local connection does not match llmux settings. Check the port or use remote connection settings."
        case .changed: "Connection changed. Retry with the current settings."
        case .timedOut: "The local connection check timed out. Retry."
        }
    }
}

enum InstalledTools {
    static func find(_ name: String) -> String? {
        let home = FileManager.default.homeDirectoryForCurrentUser.path
        return ["/opt/homebrew/bin", "/usr/local/bin", "\(home)/.local/bin", "\(home)/.cargo/bin", "\(home)/.npm-global/bin"]
            .map { "\($0)/\(name)" }.first { FileManager.default.isExecutableFile(atPath: $0) }
    }
}

struct LocalControlHandoff: Decodable {
    let endpoint: String
    private let api_key: String

    func authorize(_ request: URLRequest, expectedEndpoint: String) throws -> URLRequest {
        guard let expected = URLComponents(string: expectedEndpoint),
              expected.scheme == "http", let port = expected.port,
              ["127.0.0.1", "localhost", "::1", "[::1]"].contains(expected.host ?? ""),
              endpoint == "http://127.0.0.1:\(port)",
              let url = request.url,
              url.scheme == expected.scheme, url.host == expected.host, url.port == port,
              !api_key.isEmpty, api_key.utf8.count <= 4096,
              !api_key.contains(where: { $0.isNewline || $0 == "\r" })
        else { throw LocalConnectionError.invalid }
        var result = request
        result.setValue(api_key, forHTTPHeaderField: "x-api-key")
        return result
    }
}

actor LocalControlAuth {
    static let shared = LocalControlAuth()
    private var generation: UInt64 = 0
    func invalidate() { generation &+= 1 }

    typealias Loader = @Sendable (Int) async throws -> Data
    func authorize(
        _ request: URLRequest, endpoint: String,
        selectedEndpoint: @Sendable () -> String? = { try? LlmuxClient.current().validatedConnectionEndpoint() },
        load: Loader = LocalControlAuth.load
    ) async throws -> URLRequest {
        let start = generation
        guard selectedEndpoint() == endpoint, let port = URLComponents(string: endpoint)?.port else {
            throw LocalConnectionError.changed
        }
        // Read a fresh credential for each control operation. No secret cache,
        // persisted copy or key fingerprint can outlive a rotation.
        let data = try await load(port)
        guard generation == start, selectedEndpoint() == endpoint else { throw LocalConnectionError.changed }
        guard data.count <= 8192, let handoff = try? JSONDecoder().decode(LocalControlHandoff.self, from: data) else {
            throw LocalConnectionError.invalid
        }
        return try handoff.authorize(request, expectedEndpoint: endpoint)
    }

    nonisolated static func load(port: Int) async throws -> Data {
        guard let path = InstalledTools.find("llmux") else { throw LocalConnectionError.unavailable }
        return try await Task.detached {
            try BoundedLocalProcess.output(executable: path, arguments: ["islands-connection", "--port", String(port)])
        }.value
    }
}

/// Fixed executable + argv, no shell or PATH lookup. The bounded private CLI
/// response is never included in an error, logged, or persisted.
enum BoundedLocalProcess {
    static func output(executable: String, arguments: [String], timeout: TimeInterval = 4) throws -> Data {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: executable)
        process.arguments = arguments
        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = FileHandle.nullDevice
        do { try process.run() } catch { throw LocalConnectionError.unavailable }
        let handle = pipe.fileHandleForReading
        let fd = handle.fileDescriptor
        defer { try? handle.close() }
        let flags = fcntl(fd, F_GETFL)
        guard flags >= 0, fcntl(fd, F_SETFL, flags | O_NONBLOCK) == 0 else {
            process.terminate(); process.waitUntilExit()
            throw LocalConnectionError.unavailable
        }
        let deadline = ProcessInfo.processInfo.systemUptime + timeout
        var data = Data()
        var eof = false
        var chunk = [UInt8](repeating: 0, count: 4096)
        do {
            while true {
                let count = Darwin.read(fd, &chunk, chunk.count)
                if count > 0 {
                    guard data.count + count <= 8192 else { throw LocalConnectionError.invalid }
                    data.append(contentsOf: chunk.prefix(count))
                } else if count == 0 {
                    eof = true
                } else if errno != EAGAIN && errno != EWOULDBLOCK && errno != EINTR {
                    throw LocalConnectionError.unavailable
                }
                if eof && !process.isRunning { break }
                guard ProcessInfo.processInfo.systemUptime < deadline else { throw LocalConnectionError.timedOut }
                if count <= 0 { Thread.sleep(forTimeInterval: 0.005) }
            }
            process.waitUntilExit()
            guard process.terminationStatus == 0 else { throw LocalConnectionError.unavailable }
            return data
        } catch {
            if process.isRunning {
                process.terminate()
                Thread.sleep(forTimeInterval: 0.05)
                if process.isRunning { kill(process.processIdentifier, SIGKILL) }
                process.waitUntilExit()
            }
            throw error
        }
    }
}

/// App release versions distinguish preview (YYYY.MM.DD.HHMM) from stable.
/// Copy only: installing/updating remains an explicit Terminal action.
enum IslandsRecovery {
    static func command(appVersion: String, llmuxInstalled: Bool) -> String {
        let preview = (Int(appVersion.split(separator: ".").first ?? "") ?? 0) >= 2020
        let suffix = preview ? "-preview" : ""
        if !llmuxInstalled {
            return "brew update && brew install 2lab-ai/tap/llmux\(suffix) 2lab-ai/tap/llmux-islands\(suffix)"
        }
        return preview ? "llmux channel preview && llmux update" : "llmux channel stable && llmux update"
    }
}
