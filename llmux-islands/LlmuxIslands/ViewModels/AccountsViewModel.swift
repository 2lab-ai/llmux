import Foundation
import SwiftUI

/// Drives the accounts panel: polls `GET /llmux/status` on an interval and
/// exposes the add / remove / OAuth-login actions. Replaces agent-island's
/// credential-extraction + profile-switching view model entirely — all state
/// comes from the llmux HTTP API.
@MainActor
final class AccountsViewModel: ObservableObject {
    @Published var accounts: [LlmuxAccount] = []
    @Published var current: String?
    @Published var connection: Connection = .connecting
    @Published var lastError: String?
    @Published var login: LoginFlow?

    enum Connection: Equatable {
        case connecting
        case online
        case offline(String)
    }

    /// In-flight OAuth login the panel renders a progress card for.
    struct LoginFlow: Equatable {
        var provider: String           // "claude" | "codex"
        var phase: String              // "starting" | "pending" | "done" | "error"
        var message: String?
        var state: String?
    }

    private let client = LlmuxClient()
    private var pollTask: Task<Void, Never>?
    private let pollInterval: UInt64 = 10 * 1_000_000_000   // 10s

    func start() {
        guard pollTask == nil else { return }
        pollTask = Task { [weak self] in
            while !Task.isCancelled {
                await self?.refresh()
                try? await Task.sleep(nanoseconds: self?.pollInterval ?? 10_000_000_000)
            }
        }
    }

    func stop() {
        pollTask?.cancel()
        pollTask = nil
    }

    func refresh() async {
        do {
            let status = try await client.status()
            accounts = status.accounts
            current = status.current
            connection = .online
        } catch {
            connection = .offline(error.localizedDescription)
        }
    }

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

    /// Start a daemon-run OAuth login and poll it to completion. llmux opens the
    /// browser; we only surface progress.
    func startLogin(provider: String) async {
        login = LoginFlow(provider: provider, phase: "starting", message: "Opening browser…", state: nil)
        do {
            let started = try await client.startLogin(provider: provider)
            login?.state = started.state
            login?.phase = "pending"
            // Poll up to ~5 minutes.
            for _ in 0..<150 {
                if Task.isCancelled { return }
                try? await Task.sleep(nanoseconds: 2_000_000_000)
                guard let state = login?.state else { return }
                let result = try await client.loginStatus(state: state)
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
            }
            login?.phase = "error"
            login?.message = "timed out"
        } catch {
            login?.phase = "error"
            login?.message = error.localizedDescription
        }
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
