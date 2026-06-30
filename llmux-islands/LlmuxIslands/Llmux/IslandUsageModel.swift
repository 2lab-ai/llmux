import Foundation
import SwiftUI

/// The accounts/usage model that feeds the island's `.usage` content. Polls
/// `GET /llmux/status` and maps each llmux account onto the agent-island
/// `UsageAccountTile` so the lifted tile grid renders unchanged. Also owns the
/// add / remove / OAuth-login actions. Replaces agent-island's
/// `UsageDashboardViewModel` + the whole cauth/credential pipeline.
@MainActor
final class IslandUsageModel: ObservableObject {
    static let shared = IslandUsageModel()

    @Published var tiles: [UsageAccountTile] = []
    @Published var current: String?
    @Published var connection: Connection = .connecting
    @Published var lastError: String?
    @Published var login: LoginFlow?

    enum Connection: Equatable {
        case connecting
        case online
        case offline(String)
    }

    struct LoginFlow: Equatable {
        var provider: String       // "claude" | "codex"
        var phase: String          // "starting" | "pending" | "done" | "error"
        var message: String?
        var state: String?
    }

    // Rebuilt from the saved settings on each use so the Settings window's
    // host/port/api-key changes take effect on the next call.
    private var client: LlmuxClient { LlmuxClient.current() }
    private var pollTask: Task<Void, Never>?

    func start() {
        guard pollTask == nil else { return }
        pollTask = Task { [weak self] in
            while !Task.isCancelled {
                await self?.refresh()
                try? await Task.sleep(nanoseconds: 10_000_000_000)
            }
        }
    }

    func refresh() async {
        do {
            let status = try await client.status()
            current = status.current
            tiles = status.accounts.map(Self.tile(from:))
            connection = .online
        } catch {
            connection = .offline(error.localizedDescription)
        }
    }

    /// Map one llmux account record onto the agent-island tile model.
    static func tile(from a: LlmuxAccountRecord) -> UsageAccountTile {
        let provider: UsageProvider =
            (a.group?.lowercased() == "codex" || a.type.lowercased() == "codex") ? .codex : .claude

        let email: String? = {
            if let colon = a.name.firstIndex(of: ":") {
                return String(a.name[a.name.index(after: colon)...])
            }
            return a.name.contains("@") ? a.name : nil
        }()

        let authFailed = (a.status == "auth_failed")

        // llmux utilization is 0...1; the tile expects 0...100.
        let info = CLIUsageInfo(
            name: a.name,
            available: !authFailed,
            error: authFailed,
            fiveHourPercent: a.fiveHour.map { $0.utilization * 100 },
            sevenDayPercent: a.sevenDay.map { $0.utilization * 100 },
            fiveHourReset: a.fiveHour.flatMap { $0.resetsInSecs }.map { Date(timeIntervalSinceNow: TimeInterval($0)) },
            sevenDayReset: a.sevenDay.flatMap { $0.resetsInSecs }.map { Date(timeIntervalSinceNow: TimeInterval($0)) },
            model: nil,
            plan: nil,
            buckets: nil
        )

        let tokenRefresh: TokenRefreshInfo? = a.tokenExpiresAtMs.map {
            TokenRefreshInfo(
                expiresAt: Date(timeIntervalSince1970: TimeInterval($0) / 1000),
                lifetimeSeconds: 8 * 3600
            )
        }

        return UsageAccountTile(
            id: a.name,
            provider: provider,
            accountId: a.name,
            label: a.name,
            email: email,
            tier: nil,
            claudeIsTeam: nil,
            tokenRefresh: tokenRefresh,
            info: info,
            errorMessage: authFailed ? "auth failed — re-login" : nil,
            issue: nil
        )
    }

    // MARK: - Actions

    @discardableResult
    func addApiKey(name: String, key: String) async -> Bool {
        do {
            try await client.addApiKey(name: name.isEmpty ? nil : name, apiKey: key)
            await refresh()
            return true
        } catch {
            lastError = error.localizedDescription
            return false
        }
    }

    func remove(_ name: String) async {
        do {
            try await client.remove(name: name)
            await refresh()
        } catch {
            lastError = error.localizedDescription
        }
    }

    /// Start a daemon-run OAuth login (Claude or Codex subscription) and poll it,
    /// mirroring llmux's `a → n` add-account flow.
    func startLogin(provider: String) async {
        login = LoginFlow(provider: provider, phase: "starting", message: "Opening browser…", state: nil)
        do {
            let started = try await client.startLogin(provider: provider)
            login?.state = started.state
            login?.phase = "pending"
            var consecutiveErrors = 0
            for _ in 0..<150 {                       // ~5 min at 2s
                if Task.isCancelled { return }
                try? await Task.sleep(nanoseconds: 2_000_000_000)
                guard let state = login?.state else { return }
                do {
                    let result = try await client.loginStatus(state: state)
                    consecutiveErrors = 0
                    login?.phase = result.phase
                    if result.phase == "done" {
                        login?.message = result.account
                        await refresh()
                        return
                    }
                    if result.phase == "error" {
                        login?.message = result.error ?? "login failed"
                        return
                    }
                } catch {
                    // Tolerate transient poll failures (daemon restart, brief
                    // network blip) — only give up after several in a row.
                    consecutiveErrors += 1
                    if consecutiveErrors >= 5 {
                        login?.phase = "error"
                        login?.message = Self.friendlyError(error)
                        return
                    }
                }
            }
            login?.phase = "error"
            login?.message = "timed out"
        } catch {
            login?.phase = "error"
            login?.message = Self.friendlyError(error)
        }
    }

    /// Turn a raw HTTP error into an actionable message. A 404 on the login
    /// endpoints means the daemon predates them (added in llmux 0.2.4).
    static func friendlyError(_ error: Error) -> String {
        if case let LlmuxError.http(code, _) = error, code == 404 {
            return "This llmux daemon doesn't support adding accounts over OAuth. Update it (brew upgrade llmux) and restart (llmux restart) — needs 0.2.4+."
        }
        return error.localizedDescription
    }

    func cancelLogin() async {
        if let state = login?.state {
            await client.cancelLogin(state: state)
        }
        login = nil
    }

    func dismissLogin() {
        login = nil
    }
}
