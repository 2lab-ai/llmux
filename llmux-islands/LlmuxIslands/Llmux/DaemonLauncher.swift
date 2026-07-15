import Foundation

/// Makes the app self-sufficient: right after install, launching LlmuxIslands is
/// enough — if the configured loopback HTTP daemon isn't already running, we
/// start it on that port in the background.
///
/// Only a local, plain-HTTP loopback daemon is managed. Remote and explicit
/// HTTPS endpoints never cause local process work.
enum DaemonLauncher {
    struct LocalEndpoint: Equatable {
        let baseURL: String
        let port: Int
    }

    /// Probe the configured daemon; if it's local and unreachable, spawn
    /// `llmux server --port <configured-port> --no-tui` detached and wait
    /// briefly for that same endpoint to bind so the first status poll succeeds.
    static func ensureRunning(client: LlmuxClient) async {
        guard let endpoint = localEndpoint(for: client) else { return }
        if await isReachable(endpoint) { return }
        // The probe suspends. If the user changed connection settings while it
        // was in flight, do not start a daemon for the stale endpoint.
        guard localEndpoint(for: LlmuxClient.current()) == endpoint else { return }

        guard let exe = findBinary() else {
            NSLog("llmux-islands: llmux binary not found — cannot auto-start the daemon. Install llmux (brew install llmux).")
            return
        }
        spawnDetached(exe: exe, port: endpoint.port)

        // The daemon binds in ~1s (even with zero accounts); poll up to ~6s so
        // the model's first refresh lands on a live server instead of "offline".
        for _ in 0..<20 {
            try? await Task.sleep(nanoseconds: 300_000_000)
            if await isReachable(endpoint) { return }
        }
        NSLog("llmux-islands: spawned llmux daemon but it did not answer within ~6s (it may still be starting).")
    }

    /// Capture one validated endpoint for probing and spawning. The bundled
    /// daemon serves plain HTTP, so an explicit HTTPS endpoint is not something
    /// this adapter can satisfy and must remain untouched.
    static func localEndpoint(for client: LlmuxClient) -> LocalEndpoint? {
        guard let endpoint = try? client.validatedEndpoint(),
              (try? client.isRemoteEndpoint()) == false,
              let components = URLComponents(string: endpoint),
              components.scheme?.lowercased() == "http",
              let port = components.port,
              (1...65_535).contains(port)
        else { return nil }
        return LocalEndpoint(baseURL: endpoint, port: port)
    }

    private static func isReachable(_ endpoint: LocalEndpoint) async -> Bool {
        guard let url = URL(string: endpoint.baseURL + "/llmux/status") else { return false }
        var req = URLRequest(url: url)
        req.timeoutInterval = 1.5
        guard let (_, resp) = try? await URLSession.shared.data(for: req),
              let http = resp as? HTTPURLResponse else { return false }
        return (200..<300).contains(http.statusCode)
    }

    /// GUI apps launched from Finder don't inherit the shell PATH (no
    /// /opt/homebrew/bin), so search the common install locations directly.
    private static func findBinary() -> String? {
        let home = FileManager.default.homeDirectoryForCurrentUser.path
        let candidates = [
            "/opt/homebrew/bin/llmux",   // Homebrew (Apple Silicon)
            "/usr/local/bin/llmux",      // Homebrew (Intel) / manual
            "\(home)/.cargo/bin/llmux",  // cargo install
            "\(home)/.local/bin/llmux",  // manual
        ]
        return candidates.first { FileManager.default.isExecutableFile(atPath: $0) }
    }

    static func launchArguments(port: Int) -> [String] {
        ["server", "--port", String(port), "--no-tui"]
    }

    /// Spawn the configured daemon fully detached via `/bin/sh -c 'nohup … &'`.
    /// Every value entering the command is either a validated integer or shell
    /// quoted path. stderr is appended to the daemon's own CLI log.
    private static func spawnDetached(exe: String, port: Int) {
        let stateDir = "\(FileManager.default.homeDirectoryForCurrentUser.path)/.local/state/llmux"
        try? FileManager.default.createDirectory(atPath: stateDir, withIntermediateDirectories: true)
        let log = "\(stateDir)/server.log"
        let arguments = launchArguments(port: port).map(shq).joined(separator: " ")
        let cmd = "nohup \(shq(exe)) \(arguments) >> \(shq(log)) 2>&1 &"

        let proc = Process()
        proc.executableURL = URL(fileURLWithPath: "/bin/sh")
        proc.arguments = ["-c", cmd]
        do {
            try proc.run()
            NSLog("llmux-islands: starting llmux daemon on configured loopback port \(port)")
        } catch {
            NSLog("llmux-islands: failed to start llmux daemon: \(error.localizedDescription)")
        }
    }

    /// Single-quote a path for safe embedding in the /bin/sh command line.
    private static func shq(_ s: String) -> String {
        "'" + s.replacingOccurrences(of: "'", with: "'\\''") + "'"
    }
}
