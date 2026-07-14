import Foundation
import ServiceManagement
import SwiftUI

/// The accounts/usage model that feeds the island's `.usage` content. Polls
/// `GET /llmux/dashboard` (issue #62) — the same `accounts[]` as
/// `/llmux/status` plus analytics — and maps each llmux account onto the
/// agent-island `UsageAccountTile` so the lifted tile grid renders unchanged.
/// A daemon that explicitly reports `/llmux/dashboard` as unsupported may use
/// the legacy `/llmux/status` compatibility adapter; that response is still
/// normalized and request-correlated through Rust before reaching SwiftUI.
/// Also owns the add / remove / OAuth-login actions. Replaces agent-island's
/// `UsageDashboardViewModel` + the whole cauth/credential pipeline.
@MainActor
final class IslandUsageModel: ObservableObject {
    static let shared = IslandUsageModel()

    @Published var tiles: [UsageAccountTile] = []
    @Published var current: String?
    @Published var connection: Connection = .connecting
    @Published var lastError: String?
    @Published var login: LoginFlow?
    /// Exact versioned state emitted by the Rust semantic core. SwiftUI reads
    /// projections of this value; it is retained for receipt rendering and
    /// contract diagnostics, never persisted.
    @Published private(set) var canonicalState: SharedUiState?
    @Published private(set) var activityReceipts: [SharedActivityReceipt] = []
    @Published private(set) var verificationReceipts: [SharedVerificationReceipt] = []

    /// Per-provider Σ of `in_flight` over the daemon's accounts, feeding the
    /// closed-island label (`[claude]{n} [codex]{m}`) and the mascot jump speed.
    /// Seeded from `DemoMode.forcedInFlight` so a forced count shows before the
    /// first poll completes (and even when the daemon is unreachable).
    @Published var claudeInFlight: Int = DemoMode.forcedInFlight?.claude ?? 0
    @Published var codexInFlight: Int = DemoMode.forcedInFlight?.codex ?? 0
    @Published var grokInFlight: Int = DemoMode.forcedInFlight?.grok ?? 0

    // Dashboard analytics (issue #62 S3): published for the analytics UI
    // (Phase 2). A status-only daemon retains the last Rust-accepted analytics
    // snapshot when one exists; otherwise these begin empty.
    @Published var dashboard: LlmuxDashboard?
    @Published var totals: LlmuxDashboardTotals?
    @Published var modelUsage: [LlmuxDashboardModelUsage] = []
    @Published var clientUsage: [LlmuxDashboardClientUsage] = []
    @Published var windowed: [LlmuxDashboardWindowed] = []
    @Published var activity: LlmuxDashboardActivity?

    /// exception-beacon: the ONE worst exception the closed label shows
    /// (offline > AUTH N > LIMIT N > low-quota > debounced degraded > none)
    /// and the open panel's NEEDS ATTENTION rows — both from the same
    /// [`GlanceResolver`] run, so the two surfaces can never disagree.
    @Published var glance: GlanceSignal = .none
    @Published var attention: [GlanceAttention] = []
    /// Consecutive polls with an explicit degraded account (closed-chip
    /// debounce; resets to 0 the moment no account reports degraded).
    private var degradedStreak = 0

    /// U11/U13 health count: `auth_failed` accounts + accounts over 90% quota
    /// (`DashboardHealth.Summary.total`). Drives the banner AND the closed
    /// pill's `⚠N` badge; derived by Rust for dashboard and normalized legacy
    /// responses alike. 0 = healthy.
    @Published var healthWarningCount: Int = 0

    /// The daemon's `email_anonymous` setting from the last Rust-accepted
    /// dashboard-compatible response. `nil` = no accepted response yet, or a
    /// legacy status omitted the field → the toggle stays local-only (E7).
    /// Non-nil = the server owns the setting: it is mirrored into
    /// `AppSettings.emailAnonymousEnabled` (the @AppStorage key every
    /// EmailPixelized surface renders from) so the mosaic follows the server,
    /// and the menu toggle POSTs the flip instead of writing locally (E5/E6).
    @Published var serverEmailAnonymous: Bool?

    enum Connection: Equatable {
        case connecting
        case online
        case offline(String)
    }

    struct LoginFlow: Equatable {
        var provider: String       // "claude" | "codex" | "grok"
        var phase: String          // "starting" | "pending" | "done" | "error"
        var message: String?
        var state: String?
        /// Device-code flow (grok): verification page + code, surfaced so a
        /// REMOTE daemon's login is still completable from this machine (the
        /// daemon's own browser-open lands on the daemon host).
        var verificationUri: String?
        var userCode: String?
    }

    // Rebuilt from the saved settings on each use so the Settings window's
    // host/port/api-key changes take effect on the next call.
    private var client: LlmuxClient { LlmuxClient.current() }
    private var pollTask: Task<Void, Never>?
    private var retryTask: Task<Void, Never>?
    private var loginPollTask: Task<Void, Never>?
    private var sharedCore: SharedUiCoreRuntime?
    private var sharedCoreConfigurationKey: String?
    /// Raw daemon documents are retained only in memory and only after Rust
    /// accepted their request id. The old-status adapter uses this as a base so
    /// analytics absent from `/status` remain last-good rather than vanishing.
    private var lastAcceptedDashboardJSON: Data?
    private var lastDashboardReceivedAtMs: UInt64 = 0
    /// `false` means a status-only daemon omitted `email_anonymous`; its
    /// canonical Rust state carries the injected local preference, but no
    /// server settings endpoint should be called.
    private var serverOwnsEmailSetting: Bool?
    private var suppressCompletedLogin = false

    func start() {
        guard pollTask == nil else { return }
        pollTask = Task { [weak self] in
            await self?.startSemanticSession()
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 10_000_000_000)
                guard !Task.isCancelled else { return }
                await self?.refresh(source: "poll")
            }
        }
    }

    private func startSemanticSession() async {
        let activeClient = client
        do {
            let runtime = try semanticRuntime(for: activeClient)
            let transition = try runtime.dispatch(["type": "app_started"])
            apply(transition.state)
            await executeCoreEffects(transition.effects, runtime: runtime, client: activeClient)
        } catch {
            connection = .offline(Self.safeErrorMessage(error))
            updateGlance(records: [], offline: true)
        }
    }

    func refresh() async {
        await refresh(source: "manual")
    }

    /// Native notch/window events are reducer inputs too. The shell keeps
    /// ownership of AppKit geometry and animation; Rust owns the canonical
    /// lifecycle/navigation mirror consumed by every platform.
    func nativeWindowOpened(reason: String, size: CGSize) async {
        let normalizedReason = ["click", "hover", "notification", "usage_alert", "boot"]
            .contains(reason) ? reason : "click"
        await dispatchNativeAction([
            "type": "open_requested",
            "reason": normalizedReason,
        ])
        await nativeWindowMetricsChanged(size: size)
    }

    func nativeWindowClosed() async {
        await dispatchNativeAction(["type": "close_requested"])
        // The macOS shell resets to Usage when it closes; mirror that choice
        // explicitly so canonical navigation cannot remain on Statistics.
        await dispatchNativeAction([
            "type": "navigation_selected",
            "navigation": "usage",
        ])
    }

    func nativeNavigationSelected(_ navigation: String, size: CGSize) async {
        guard ["usage", "statistics", "menu"].contains(navigation) else { return }
        await dispatchNativeAction([
            "type": "navigation_selected",
            "navigation": navigation,
        ])
        await nativeWindowMetricsChanged(size: size)
    }

    func nativeWindowMetricsChanged(size: CGSize) async {
        await dispatchNativeAction([
            "type": "window_metrics_changed",
            "width": Self.metric(size.width),
            "content_height": Self.metric(size.height),
        ])
    }

    private func dispatchNativeAction(_ action: [String: Any]) async {
        let activeClient = client
        do {
            let runtime = try semanticRuntime(for: activeClient)
            let transition = try runtime.dispatch(action)
            guard sharedCore === runtime else { return }
            apply(transition.state)
            await executeCoreEffects(transition.effects, runtime: runtime, client: activeClient)
        } catch {
            lastError = Self.safeErrorMessage(error)
        }
    }

    private func refresh(source: String) async {
        let activeClient = client
        do {
            let runtime = try semanticRuntime(for: activeClient)
            let transition = try runtime.dispatch([
                "type": "refresh_requested",
                "source": source,
            ])
            apply(transition.state)
            await executeCoreEffects(transition.effects, runtime: runtime, client: activeClient)
        } catch {
            connection = .offline(Self.safeErrorMessage(error))
            updateGlance(records: [], offline: true)
        }
    }

    private func apply(_ state: SharedUiState) {
        canonicalState = state
        current = state.usage.currentByGroup["claude"] ?? state.usage.currentByGroup.values.first
        tiles = state.usage.accounts.enumerated().map { index, account in
            let tile = account.usageTile
            return DemoMode.isActive ? Self.demoMasked(tile, index: index, preservingActionID: true) : tile
        }
        claudeInFlight = DemoMode.forcedInFlight?.claude ?? Int(state.usage.providerInFlight["claude"] ?? 0)
        codexInFlight = DemoMode.forcedInFlight?.codex ?? Int(state.usage.providerInFlight["codex"] ?? 0)
        grokInFlight = DemoMode.forcedInFlight?.grok ?? Int(state.usage.providerInFlight["grok"] ?? 0)
        healthWarningCount = state.healthWarningCount
        let dash = state.dashboardProjection
        dashboard = dash
        totals = dash.totals
        modelUsage = dash.modelUsage
        clientUsage = dash.clientUsage
        windowed = dash.windowed
        activity = dash.activity
        activityReceipts = state.statistics.activityReceipts
        verificationReceipts = state.verificationReceipts
        connection = state.connectionState
        applyLogin(state.usage.login)
        if state.connection.lastSuccessMs != nil, serverOwnsEmailSetting != false {
            applyServerEmailAnonymous(state.settings.emailAnonymous)
        } else if serverOwnsEmailSetting == false {
            serverEmailAnonymous = nil
        }
    }

    /// Installs a state produced by the real Rust bridge for offscreen visual
    /// evidence. The environment gate keeps this path unreachable in a normal
    /// app launch; snapshot mode still exercises the production projection and
    /// receipt renderers instead of maintaining a second UI-only fixture.
    func installSnapshotFixtureState(_ state: SharedUiState) {
        guard SnapshotMode.isActive else { return }
        apply(state)
    }

    private func semanticRuntime(for client: LlmuxClient) throws -> SharedUiCoreRuntime {
        // A cleared remote credential is still a valid saved configuration.
        // The semantic state must exist so it can report unauthenticated; the
        // HTTP client itself remains fail-closed until a replacement is saved.
        let endpoint = try client.validatedConnectionEndpoint()
        let remote = try client.isRemoteConnectionEndpoint()
        let apiKeyConfigured = !(client.apiKey?.isEmpty ?? true)
        let screenID = ScreenSelector.shared.selectedScreen
            .flatMap { $0.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as? CGDirectDisplayID }
            .map(String.init) ?? "auto"
        let key = "\(endpoint)|\(remote)|\(apiKeyConfigured)"
        if sharedCoreConfigurationKey != key || sharedCore == nil {
            sharedCore = try SharedUiCoreRuntime(configuration: .init(
                endpointDisplay: endpoint,
                remote: remote,
                authenticated: !remote || apiKeyConfigured,
                apiKeyConfigured: apiKeyConfigured,
                selectedScreenID: screenID,
                soundID: AppSettings.notificationSound.rawValue,
                showFableWeekly: AppSettings.showFableWeekly,
                presentation: "regular"
            ))
            sharedCoreConfigurationKey = key
            lastAcceptedDashboardJSON = nil
            lastDashboardReceivedAtMs = 0
            serverOwnsEmailSetting = nil
            serverEmailAnonymous = nil
        }
        guard let sharedCore else { throw SharedUiCoreError.creationFailed }
        return sharedCore
    }

    private func handleSchedulingEffects(_ effects: [SharedCoreEffect]) {
        for effect in effects {
            switch effect.type {
            case "schedule_dashboard_retry":
                guard let retryAtMs = effect.retryAtMs else { continue }
                retryTask?.cancel()
                let nowMs = Self.nowMs()
                let delayMs = retryAtMs >= nowMs ? retryAtMs - nowMs : 0
                retryTask = Task { [weak self] in
                    try? await Task.sleep(nanoseconds: delayMs * 1_000_000)
                    guard !Task.isCancelled else { return }
                    await self?.refresh(source: "retry")
                }
            case "cancel_dashboard_retry":
                retryTask?.cancel()
                retryTask = nil
            default:
                break
            }
        }
    }

    private func executeCoreEffects(
        _ effects: [SharedCoreEffect],
        runtime: SharedUiCoreRuntime,
        client: LlmuxClient
    ) async {
        guard sharedCore === runtime else { return }
        handleSchedulingEffects(effects)
        for effect in effects {
            guard sharedCore === runtime else { return }
            switch effect.type {
            case "ensure_local_daemon":
                // The executor receives the same endpoint snapshot that
                // configured this runtime; the launcher rechecks mutable
                // settings after its probe before starting anything.
                await DaemonLauncher.ensureRunning(client: client)
            case "fetch_dashboard":
                guard let requestID = effect.dashboardRequestID else { continue }
                await executeDashboardRequest(requestID, runtime: runtime, client: client)
            case "update_tray":
                // macOS has no NSStatusItem tray shell. The native closed-notch
                // label observes the already-applied canonical provider counts.
                break
            default:
                break
            }
        }
    }

    private func executeDashboardRequest(
        _ requestID: String,
        runtime: SharedUiCoreRuntime,
        client: LlmuxClient
    ) async {
        let bytes: Data
        do {
            bytes = try await client.dashboardData()
        } catch {
            if let llmuxError = error as? LlmuxError,
               llmuxError.isUnsupportedDashboardEndpoint {
                await executeLegacyStatusRequest(requestID, runtime: runtime, client: client)
            } else {
                await failDashboardRequest(requestID, error: error, runtime: runtime, client: client)
            }
            return
        }

        guard sharedCore === runtime else { return }
        let receivedAtMs = nextDashboardReceivedAtMs()
        do {
            // Rust requires a concrete privacy setting. For an older daemon
            // that omitted this additive key, inject the local AppSettings
            // value while keeping ownership explicitly local-only.
            let normalized = try LlmuxDashboardWireNormalizer.normalize(
                bytes,
                localEmailAnonymous: AppSettings.emailAnonymousEnabled
            )
            let completed = try runtime.applyDashboard(
                requestID: requestID,
                dashboardJSON: normalized.dashboardJSON,
                receivedAtMs: receivedAtMs
            )
            guard sharedCore === runtime,
                  completed.state.connection.lastSuccessMs == receivedAtMs
            else { return }
            lastAcceptedDashboardJSON = normalized.dashboardJSON
            serverOwnsEmailSetting = normalized.serverOwnsEmailSetting
            apply(completed.state)
            updateGlance(from: completed.state)
            await executeCoreEffects(completed.effects, runtime: runtime, client: client)
        } catch {
            // A 2xx response with an invalid dashboard is a protocol failure,
            // never evidence that the endpoint is unsupported.
            await failDashboardRequest(requestID, error: error, runtime: runtime, client: client)
        }
    }

    private func executeLegacyStatusRequest(
        _ requestID: String,
        runtime: SharedUiCoreRuntime,
        client: LlmuxClient
    ) async {
        do {
            let statusBytes = try await client.statusData()
            guard sharedCore === runtime else { return }
            let legacyServerOwnsEmail = try JSONDecoder()
                .decode(LlmuxStatus.self, from: statusBytes)
                .emailAnonymous != nil
            let receivedAtMs = nextDashboardReceivedAtMs()
            let endpointPort = URLComponents(string: try client.validatedEndpoint())?.port
            let normalized = try LegacyStatusDashboardAdapter.dashboardData(
                from: statusBytes,
                previousDashboardData: lastAcceptedDashboardJSON,
                endpointPort: endpointPort,
                receivedAtMs: receivedAtMs,
                emailAnonymous: AppSettings.emailAnonymousEnabled,
                showFableWeekly: AppSettings.showFableWeekly
            )
            let completed = try runtime.applyDashboard(
                requestID: requestID,
                dashboardJSON: normalized,
                receivedAtMs: receivedAtMs
            )
            guard sharedCore === runtime,
                  completed.state.connection.lastSuccessMs == receivedAtMs
            else { return }
            lastAcceptedDashboardJSON = normalized
            serverOwnsEmailSetting = legacyServerOwnsEmail
            apply(completed.state)
            updateGlance(from: completed.state)
            await executeCoreEffects(completed.effects, runtime: runtime, client: client)
        } catch {
            await failDashboardRequest(requestID, error: error, runtime: runtime, client: client)
        }
    }

    private func failDashboardRequest(
        _ requestID: String,
        error: Error,
        runtime: SharedUiCoreRuntime,
        client: LlmuxClient
    ) async {
        guard sharedCore === runtime else { return }
        do {
            let failed = try runtime.dispatch([
                "type": "dashboard_failed",
                "request_id": requestID,
                "error": Self.safeErrorMessage(error),
                "failed_at_ms": Self.nowMs(),
            ])
            guard sharedCore === runtime else { return }
            apply(failed.state)
            updateGlance(records: [], offline: true)
            await executeCoreEffects(failed.effects, runtime: runtime, client: client)
        } catch {
            connection = .offline(Self.safeErrorMessage(error))
            updateGlance(records: [], offline: true)
        }
    }

    private func nextDashboardReceivedAtMs() -> UInt64 {
        let now = Self.nowMs()
        let next = lastDashboardReceivedAtMs == .max ? UInt64.max : lastDashboardReceivedAtMs + 1
        lastDashboardReceivedAtMs = max(now, next)
        return lastDashboardReceivedAtMs
    }

    private func applyLogin(_ shared: SharedLoginState) {
        if shared.phase == "idle" || shared.phase == "cancelled" {
            login = nil
            return
        }
        if suppressCompletedLogin && (shared.phase == "done" || shared.phase == "error") { return }
        login = LoginFlow(
            provider: shared.provider ?? "claude",
            phase: shared.phase,
            message: shared.message,
            state: shared.state,
            verificationUri: shared.verificationUri,
            userCode: shared.userCode
        )
    }

    private static func nowMs() -> UInt64 {
        UInt64(max(0, Date().timeIntervalSince1970 * 1_000))
    }

    private static func metric(_ value: CGFloat) -> UInt32 {
        guard value.isFinite, value > 0 else { return 0 }
        return UInt32(min(value.rounded(), CGFloat(UInt32.max)))
    }

    private func updateGlance(from state: SharedUiState) {
        let records = state.dashboardProjection.accounts.map(\.statusRecord)
        updateGlance(records: records, offline: false)
    }

    /// Run the beacon resolver over one Rust-accepted poll or an offline
    /// failure. Demo mode maps account names onto the same stable fakes the
    /// tiles use, so an attention row never leaks a real email into a recording.
    private func updateGlance(records: [LlmuxAccountRecord], offline: Bool) {
        if !offline, records.contains(where: { $0.status == "degraded" }) {
            degradedStreak += 1
        } else {
            degradedStreak = 0
        }
        let output = GlanceResolver.resolve(
            records: records,
            offline: offline,
            degradedStreak: degradedStreak,
            displayName: { index, name in
                DemoMode.isActive ? DemoMode.fakeEmail(index: index) : name
            }
        )
        glance = output.signal
        attention = output.attention
    }

    /// Fold the daemon's `email_anonymous` (if it reports one) into local
    /// state: the server value WINS and is mirrored into the @AppStorage key
    /// as a cache, so every EmailPixelized surface re-renders from it and an
    /// out-of-band flip (TUI, curl) propagates on the next poll. An old
    /// daemon (`nil`) leaves the local key untouched — today's local-only
    /// behavior (E7).
    private func applyServerEmailAnonymous(_ server: Bool?) {
        serverEmailAnonymous = server
        if let server, AppSettings.emailAnonymousEnabled != server {
            AppSettings.emailAnonymousEnabled = server
        }
    }

    /// The menu's "Email anonymous" toggle action. New daemon: POST the flip
    /// to `/llmux/settings` and reflect the ack (the daemon persists it, and
    /// every other client/TUI follows). Old daemon or never connected: flip
    /// the local key only, exactly as before the setting became server-owned.
    func toggleEmailAnonymous() async {
        let current = serverEmailAnonymous ?? AppSettings.emailAnonymousEnabled
        let enabled = !current
        let id = Self.operationID(prefix: "settings")
        _ = await performSemanticOperation(
            id: id,
            startAction: [
                "type": "settings_changed",
                "id": id,
                "email_anonymous": enabled,
                "started_at_ms": Self.nowMs(),
            ]
        )
    }

    /// Replace an account's real email/label with a stable fake so a public demo
    /// recording never leaks account names. Usage numbers stay real (not PII).
    static func demoMasked(
        _ tile: UsageAccountTile,
        index: Int,
        preservingActionID: Bool = false
    ) -> UsageAccountTile {
        let fake = DemoMode.fakeEmail(index: index)
        return UsageAccountTile(
            id: preservingActionID ? tile.id : fake,
            provider: tile.provider,
            accountId: preservingActionID ? tile.accountId : fake,
            label: fake,
            email: fake,
            tier: tile.tier,
            claudeIsTeam: tile.claudeIsTeam,
            tokenRefresh: tile.tokenRefresh,
            info: tile.info,
            errorMessage: tile.errorMessage,
            issue: tile.issue,
            paused: tile.paused
        )
    }

    // MARK: - Actions

    private struct ExecutorAck {
        let outcome: String
        let message: String

        static func succeeded(_ message: String) -> Self { .init(outcome: "succeeded", message: message) }
        static func noChange(_ message: String) -> Self { .init(outcome: "no_change", message: message) }
        static func failed(_ message: String = "operation failed") -> Self { .init(outcome: "failed", message: message) }
    }

    /// Secret-bearing values live only in the stack frame that executes a
    /// transient effect. They are never encoded into an action, UiState,
    /// receipt, log, or published model property.
    private enum ExecutorContext {
        case none
        case addAPIKey(String)
        case connection(
            host: String,
            port: Int,
            apiKey: String,
            apiKeyWasExplicitlyCleared: Bool,
            endpoint: String
        )
        case maintenance(CLIRunner)

        var switchesConnection: Bool {
            if case .connection = self { return true }
            return false
        }
    }

    @discardableResult
    func addApiKey(name: String, key: String) async -> Bool {
        let trimmedName = name.trimmingCharacters(in: .whitespacesAndNewlines)
        let result = await performOperation(
            prefix: "add",
            request: [
                "kind": "add_account",
                "name": trimmedName.isEmpty ? NSNull() : trimmedName,
                "has_api_key": !key.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
            ],
            targetDisplay: trimmedName.isEmpty ? nil : trimmedName,
            context: .addAPIKey(key)
        )
        return result.outcome == "succeeded" || result.outcome == "no_change"
    }

    func remove(_ accountHandle: String) async {
        let display = tiles.first { $0.accountId == accountHandle }?.label
        _ = await performOperation(
            prefix: "remove",
            request: [
                "kind": "remove_account",
                "account_id": accountHandle,
                "confirmed": true,
            ],
            targetDisplay: display
        )
    }

    /// Pause/resume one account (context menu). The daemon persists the flag
    /// (`paused_accounts`) and the scheduler skips the account until resumed;
    /// the sepia tile state follows on the next refresh.
    func setPaused(_ accountHandle: String, paused: Bool) async {
        let display = tiles.first { $0.accountId == accountHandle }?.label
        _ = await performOperation(
            prefix: paused ? "pause" : "resume",
            request: [
                "kind": "pause_account",
                "account_id": accountHandle,
                "paused": paused,
            ],
            targetDisplay: display
        )
    }

    @discardableResult
    func upsertEvent(_ event: LlmuxEvent) async -> Bool {
        let result = await performOperation(
            prefix: "event",
            request: [
                "kind": "upsert_event",
                "event": event.jsonObject,
            ],
            targetDisplay: event.id
        )
        return result.outcome == "succeeded" || result.outcome == "no_change"
    }

    @discardableResult
    func removeEvent(id eventID: String) async -> Bool {
        let result = await performOperation(
            prefix: "event-remove",
            request: [
                "kind": "remove_event",
                "event_id": eventID,
            ],
            targetDisplay: eventID
        )
        return result.outcome == "succeeded" || result.outcome == "no_change"
    }

    func refreshEvents() async -> [LlmuxEvent] {
        await refresh()
        return dashboard?.events ?? []
    }

    func setShowFableWeekly(_ enabled: Bool) async {
        _ = await performOperation(
            prefix: "fable",
            request: ["kind": "persist_show_fable", "enabled": enabled],
            targetDisplay: enabled ? "shown" : "hidden"
        )
    }

    func selectScreen(_ screen: NSScreen?) async {
        let identifier = screen.flatMap(Self.screenIdentifier) ?? "auto"
        _ = await performOperation(
            prefix: "screen",
            request: ["kind": "persist_screen", "id": identifier],
            targetDisplay: screen?.localizedName ?? "Automatic"
        )
    }

    func selectSound(_ sound: NotificationSound) async {
        _ = await performOperation(
            prefix: "sound",
            request: ["kind": "persist_sound", "id": sound.rawValue],
            targetDisplay: sound.rawValue
        )
    }

    @discardableResult
    func applyConnection(
        host rawHost: String,
        port: Int,
        apiKeyIntent: ConnectionApiKeyIntent
    ) async -> Bool {
        do {
            let plan = try ConnectionPersistencePlan.build(
                host: rawHost,
                port: port,
                apiKeyIntent: apiKeyIntent,
                existingHost: LlmuxSettings.host,
                existingPort: LlmuxSettings.port,
                existingKey: LlmuxSettings.apiKey,
                existingKeyWasExplicitlyCleared: LlmuxSettings.apiKeyWasExplicitlyCleared
            )
            let context: ExecutorContext = .connection(
                host: plan.host,
                port: plan.port,
                apiKey: plan.apiKey,
                apiKeyWasExplicitlyCleared: plan.apiKeyWasExplicitlyCleared,
                endpoint: plan.endpoint
            )
            let result = await performOperation(
                prefix: "connection",
                request: [
                    "kind": "persist_connection",
                    "endpoint": plan.endpoint,
                    "api_key_configured": !plan.apiKey.isEmpty,
                ],
                targetDisplay: plan.endpoint,
                context: context
            )
            return result.outcome == "succeeded" || result.outcome == "no_change"
        } catch {
            lastError = Self.safeErrorMessage(error)
            return false
        }
    }

    @discardableResult
    func setLaunchAtLogin(_ enabled: Bool) async -> Bool {
        let result = await performOperation(
            prefix: "autostart",
            request: ["kind": "set_autostart", "enabled": enabled],
            targetDisplay: enabled ? "enabled" : "disabled"
        )
        return result.outcome == "succeeded" || result.outcome == "no_change"
    }

    func runMaintenanceUpdate(runner: CLIRunner = CLIRunner()) async -> UpdateOutcome {
        let result = await performOperation(
            prefix: "maintenance",
            request: [
                "kind": "run_maintenance",
                "command": ["kind": "update"],
            ],
            targetDisplay: "update",
            context: .maintenance(runner)
        )
        return Self.updateOutcome(from: result)
    }

    func changeReleaseChannel(
        to channel: ReleaseChannel,
        runner: CLIRunner = CLIRunner()
    ) async -> UpdateOutcome {
        let result = await performOperation(
            prefix: "maintenance",
            request: [
                "kind": "run_maintenance",
                "command": ["kind": "change_channel", "channel": channel.rawValue],
            ],
            targetDisplay: channel.label,
            context: .maintenance(runner)
        )
        return Self.updateOutcome(from: result)
    }

    private func performOperation(
        prefix: String,
        request: [String: Any],
        targetDisplay: String?,
        context: ExecutorContext = .none
    ) async -> ExecutorAck {
        let id = Self.operationID(prefix: prefix)
        var action: [String: Any] = [
            "type": "operation_started",
            "id": id,
            "request": request,
            "started_at_ms": Self.nowMs(),
        ]
        if let targetDisplay { action["target_display"] = targetDisplay }
        return await performSemanticOperation(
            id: id,
            startAction: action,
            context: context
        )
    }

    private func performSemanticOperation(
        id: String,
        startAction: [String: Any],
        context: ExecutorContext = .none
    ) async -> ExecutorAck {
        let activeClient = client
        do {
            let runtime = try semanticRuntime(for: activeClient)
            let started = try runtime.dispatch(startAction)
            apply(started.state)
            handleSchedulingEffects(started.effects)
            guard let effect = started.effects.first(where: { $0.operationID == id }) else {
                return finishResult(id: id, fallback: .failed("operation was not accepted"))
            }

            let executed: ExecutorAck
            do {
                executed = try await executeOperationEffect(
                    effect,
                    context: context,
                    client: activeClient
                )
            } catch {
                executed = .failed(Self.safeErrorMessage(error))
            }
            let finishAction: [String: Any] = [
                "type": "operation_finished",
                "id": id,
                "outcome": executed.outcome,
                "message": executed.message,
                "finished_at_ms": Self.nowMs(),
            ]
            let finished = try runtime.dispatch(finishAction)
            apply(finished.state)

            if context.switchesConnection {
                // The executor has atomically installed the new endpoint. The
                // old core must close its in-flight operation, but none of its
                // refresh effects may hydrate state from the previous endpoint.
                // Replay the already-executed semantic transaction into a core
                // configured for the new connection, then refresh there. This
                // keeps the verification receipt while making the new endpoint
                // metadata authoritative immediately.
                sharedCore = nil
                sharedCoreConfigurationKey = nil
                let updatedClient = client
                let updatedRuntime = try semanticRuntime(for: updatedClient)
                let rebound = try updatedRuntime.dispatch(startAction)
                apply(rebound.state)
                guard rebound.effects.contains(where: { $0.operationID == id }) else {
                    return finishResult(id: id, fallback: .failed("connection change was not accepted"))
                }
                let reboundFinished = try updatedRuntime.dispatch(finishAction)
                apply(reboundFinished.state)
                // Re-enter the startup lifecycle for the newly configured
                // runtime. The rebound finish already owns the refresh ID, so
                // this contributes EnsureLocalDaemon (when local) without
                // issuing a duplicate fetch; execute it before that refresh.
                let reboundStarted = try updatedRuntime.dispatch(["type": "app_started"])
                apply(reboundStarted.state)
                await executeCoreEffects(
                    reboundStarted.effects,
                    runtime: updatedRuntime,
                    client: updatedClient
                )
                await executeCoreEffects(
                    reboundFinished.effects,
                    runtime: updatedRuntime,
                    client: updatedClient
                )
            } else {
                await executeCoreEffects(finished.effects, runtime: runtime, client: activeClient)
            }
            return finishResult(id: id, fallback: executed)
        } catch {
            let result = ExecutorAck.failed(Self.safeErrorMessage(error))
            lastError = result.message
            return result
        }
    }

    private func finishResult(id: String, fallback: ExecutorAck) -> ExecutorAck {
        guard let receipt = canonicalState?.verificationReceipts.last(where: { $0.id == id }) else {
            if fallback.outcome == "failed" { lastError = fallback.message }
            return fallback
        }
        let result = ExecutorAck(outcome: receipt.outcome, message: receipt.message)
        lastError = result.outcome == "failed" ? result.message : nil
        return result
    }

    private func executeOperationEffect(
        _ effect: SharedCoreEffect,
        context: ExecutorContext,
        client: LlmuxClient
    ) async throws -> ExecutorAck {
        switch effect.type {
        case "run_operation":
            guard let request = effect.request else { throw LlmuxError.invalidResponse }
            switch request.kind {
            case "add_account":
                guard request.apiKeyRequired == true,
                      case let .addAPIKey(apiKey) = context,
                      !apiKey.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                else { throw LlmuxError.invalidResponse }
                let added = try await client.addApiKey(name: request.name, apiKey: apiKey)
                return added ? .succeeded("account added") : .noChange("account already exists")
            case "pause_account":
                guard let rawAccountID = request.accountID, let paused = request.paused else {
                    throw LlmuxError.invalidResponse
                }
                try await client.setPaused(name: rawAccountID, paused: paused)
                return .succeeded(paused ? "account paused" : "account resumed")
            case "remove_account":
                guard let rawAccountID = request.accountID, request.confirmed == true else {
                    throw LlmuxError.invalidResponse
                }
                try await client.remove(name: rawAccountID)
                return .succeeded("account removed")
            default:
                throw LlmuxError.invalidResponse
            }

        case "update_settings":
            guard let enabled = effect.emailAnonymous else { throw LlmuxError.invalidResponse }
            if serverOwnsEmailSetting != true {
                // Old status-only daemons do not own this setting. The action
                // and receipt still pass through Rust; only the transient
                // platform executor writes the established local preference.
                AppSettings.emailAnonymousEnabled = enabled
                return .succeeded("settings updated locally")
            }
            let acknowledged = try await client.setEmailAnonymous(enabled)
            guard acknowledged == enabled else { throw LlmuxError.invalidResponse }
            return .succeeded("settings updated")

        case "upsert_event":
            guard let event = effect.event else { throw LlmuxError.invalidResponse }
            let value = LlmuxEvent(id: event.id, from: event.from, to: event.to, content: event.content)
            _ = try await client.upsertEvent(value)
            return .succeeded("event updated")

        case "remove_event":
            guard let eventID = effect.eventID else { throw LlmuxError.invalidResponse }
            _ = try await client.removeEvent(id: eventID)
            return .succeeded("event removed")

        case "persist_settings":
            guard let change = effect.change else { throw LlmuxError.invalidResponse }
            return try persistLocalSettings(change, context: context)

        case "set_autostart":
            guard let enabled = effect.enabled else { throw LlmuxError.invalidResponse }
            let currentlyEnabled = SMAppService.mainApp.status == .enabled
            if currentlyEnabled == enabled {
                return .noChange(enabled ? "launch at login already enabled" : "launch at login already disabled")
            }
            if enabled {
                try SMAppService.mainApp.register()
            } else {
                try await SMAppService.mainApp.unregister()
            }
            return .succeeded(enabled ? "launch at login enabled" : "launch at login disabled")

        case "run_maintenance":
            guard let command = effect.command else { throw LlmuxError.invalidResponse }
            let runner: CLIRunner
            if case let .maintenance(value) = context { runner = value } else { runner = CLIRunner() }
            let outcome: UpdateOutcome
            switch command.kind {
            case "update":
                outcome = await runner.update()
            case "change_channel":
                guard let rawChannel = command.channel,
                      let channel = ReleaseChannel(rawValue: rawChannel)
                else { throw LlmuxError.invalidResponse }
                outcome = await runner.setChannel(channel)
            default:
                throw LlmuxError.invalidResponse
            }
            switch outcome {
            case .alreadyUpToDate:
                return .noChange(outcome.summary)
            case .updated:
                return .succeeded(outcome.summary)
            case .failed:
                return .failed(outcome.summary)
            }

        default:
            throw LlmuxError.invalidResponse
        }
    }

    private func persistLocalSettings(
        _ change: SharedCoreLocalSettingsChange,
        context: ExecutorContext
    ) throws -> ExecutorAck {
        switch change.kind {
        case "screen_selected":
            guard let id = change.id else { throw LlmuxError.invalidResponse }
            if id == "auto" {
                ScreenSelector.shared.selectAutomatic()
            } else {
                guard let screen = ScreenSelector.shared.availableScreens.first(where: {
                    Self.screenIdentifier($0) == id
                }) else { throw LlmuxError.invalidResponse }
                ScreenSelector.shared.selectScreen(screen)
            }
            NotificationCenter.default.post(
                name: NSApplication.didChangeScreenParametersNotification,
                object: nil
            )
            return .succeeded("screen selection saved")

        case "sound_selected":
            guard let id = change.id, let sound = NotificationSound(rawValue: id) else {
                throw LlmuxError.invalidResponse
            }
            AppSettings.notificationSound = sound
            return .succeeded("notification sound saved")

        case "show_fable":
            guard let enabled = change.enabled else { throw LlmuxError.invalidResponse }
            AppSettings.showFableWeekly = enabled
            return .succeeded("Fable weekly visibility saved")

        case "connection_applied":
            guard case let .connection(
                host,
                port,
                apiKey,
                apiKeyWasExplicitlyCleared,
                endpoint
            ) = context,
                  change.endpoint == endpoint,
                  change.apiKeyConfigured == !apiKey.isEmpty
            else { throw LlmuxError.invalidResponse }
            // Clear the authorization marker before changing endpoints, then
            // restore it only after the whole validated transaction lands.
            LlmuxSettings.apiKey = apiKey
            LlmuxSettings.host = host
            LlmuxSettings.port = port
            LlmuxSettings.apiKeyWasExplicitlyCleared = apiKeyWasExplicitlyCleared
            return .succeeded("connection settings saved")

        default:
            throw LlmuxError.invalidResponse
        }
    }

    private static func screenIdentifier(_ screen: NSScreen) -> String? {
        (screen.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as? CGDirectDisplayID)
            .map(String.init)
    }

    private static func operationID(prefix: String) -> String {
        "\(prefix)-\(UUID().uuidString.lowercased())"
    }

    private static func updateOutcome(from result: ExecutorAck) -> UpdateOutcome {
        switch result.outcome {
        case "succeeded":
            return .updated(version: CLIParse.extractVersion(from: result.message))
        case "no_change":
            return .alreadyUpToDate
        default:
            return .failed(message: result.message)
        }
    }

    /// Start a daemon-run OAuth login. Raw daemon state is consumed only from
    /// transient effects; the published login state contains Rust's safe
    /// `"active"` marker instead.
    func startLogin(provider: String) async {
        let activeClient = client
        do {
            let runtime = try semanticRuntime(for: activeClient)
            let operationID = Self.operationID(prefix: "login")
            let transition = try runtime.dispatch([
                "type": "login_started",
                "operation_id": operationID,
                "provider": provider,
                "started_at_ms": Self.nowMs(),
            ])
            apply(transition.state)
            await consumeLoginEffects(transition.effects, runtime: runtime, client: activeClient)
        } catch {
            lastError = Self.safeErrorMessage(error)
        }
    }

    private func consumeLoginEffects(
        _ effects: [SharedCoreEffect],
        runtime: SharedUiCoreRuntime,
        client: LlmuxClient
    ) async {
        handleSchedulingEffects(effects)
        for effect in effects {
            switch effect.type {
            case "start_login":
                guard let operationID = effect.operationID, let provider = effect.provider else { continue }
                // Only an accepted reducer transition emits this effect. A
                // rejected second start must leave the active login's poll
                // task untouched so it can still terminate or time out.
                suppressCompletedLogin = false
                loginPollTask?.cancel()
                do {
                    let started = try await client.startLogin(provider: provider)
                    await reduceLoginStatus(
                        operationID: operationID,
                        status: [
                            "phase": "pending",
                            "state": started.state,
                            "message": "waiting for login",
                        ],
                        runtime: runtime,
                        client: client
                    )
                } catch {
                    await reduceLoginStatus(
                        operationID: operationID,
                        status: ["phase": "failed", "message": Self.friendlyError(error)],
                        runtime: runtime,
                        client: client
                    )
                }

            case "poll_login":
                guard let operationID = effect.operationID, let rawState = effect.state else { continue }
                loginPollTask?.cancel()
                loginPollTask = Task { [weak self] in
                    try? await Task.sleep(nanoseconds: 2_000_000_000)
                    guard !Task.isCancelled else { return }
                    await self?.pollLogin(
                        operationID: operationID,
                        rawState: rawState,
                        runtime: runtime,
                        client: client
                    )
                }

            case "stop_login_poll":
                loginPollTask?.cancel()
                loginPollTask = nil

            case "cancel_login":
                guard let operationID = effect.operationID, let rawState = effect.state else { continue }
                loginPollTask?.cancel()
                do {
                    let cancelled = try await client.cancelLogin(state: rawState)
                    await reduceLoginStatus(
                        operationID: operationID,
                        status: [
                            "phase": cancelled ? "cancellation_acknowledged" : "cancellation_failed",
                            "message": cancelled ? "login cancelled" : "login cancellation was not applied",
                        ],
                        runtime: runtime,
                        client: client
                    )
                } catch {
                    await reduceLoginStatus(
                        operationID: operationID,
                        status: ["phase": "cancellation_failed", "message": "login cancellation failed"],
                        runtime: runtime,
                        client: client
                    )
                }

            default:
                break
            }
        }
        await executeCoreEffects(effects, runtime: runtime, client: client)
    }

    private func pollLogin(
        operationID: String,
        rawState: String,
        runtime: SharedUiCoreRuntime,
        client: LlmuxClient
    ) async {
        let status: [String: Any]
        do {
            let response = try await client.loginStatus(state: rawState)
            switch response.phase {
            case "pending":
                var pending: [String: Any] = ["phase": "pending", "state": rawState]
                if let uri = response.verificationUri { pending["verification_uri"] = uri }
                if let code = response.userCode { pending["user_code"] = code }
                status = pending
            case "done":
                var succeeded: [String: Any] = ["phase": "succeeded", "message": "login succeeded"]
                if let account = response.account { succeeded["target_display"] = account }
                status = succeeded
            case "error":
                status = ["phase": "failed", "message": "login failed"]
            default:
                status = ["phase": "failed", "message": "login status was invalid"]
            }
        } catch {
            // Keep the raw correlation state only in the next semantic action;
            // Rust enforces the five-minute deadline and retains safe device
            // flow fields while transient network errors are retried.
            status = [
                "phase": "pending",
                "state": rawState,
                "message": "login status unavailable; retrying",
            ]
        }
        await reduceLoginStatus(
            operationID: operationID,
            status: status,
            runtime: runtime,
            client: client
        )
    }

    private func reduceLoginStatus(
        operationID: String,
        status: [String: Any],
        runtime: SharedUiCoreRuntime,
        client: LlmuxClient
    ) async {
        do {
            let transition = try runtime.dispatch([
                "type": "login_status_received",
                "operation_id": operationID,
                "status": status,
                "at_ms": Self.nowMs(),
            ])
            apply(transition.state)
            await consumeLoginEffects(transition.effects, runtime: runtime, client: client)
        } catch {
            lastError = Self.safeErrorMessage(error)
        }
    }

    private static func safeErrorMessage(_ error: Error) -> String {
        if let error = error as? LlmuxError {
            return error.localizedDescription
        }
        if let error = error as? SharedUiCoreError {
            return error.localizedDescription
        }
        return "The llmux operation failed."
    }

    /// Turn a raw HTTP error into an actionable message. A 404 on the login
    /// endpoints means the daemon predates them (added in llmux 0.2.4).
    static func friendlyError(_ error: Error) -> String {
        if case let LlmuxError.http(code, _) = error, code == 404 {
            return "This llmux daemon doesn't support adding accounts over OAuth. Update it (brew upgrade llmux) and restart (llmux restart) — needs 0.2.4+."
        }
        return safeErrorMessage(error)
    }

    func cancelLogin() async {
        guard let operationID = canonicalState?.operation?.id else { return }
        do {
            let activeClient = client
            let runtime = try semanticRuntime(for: activeClient)
            let transition = try runtime.dispatch([
                "type": "login_cancel_requested",
                "operation_id": operationID,
            ])
            apply(transition.state)
            await consumeLoginEffects(transition.effects, runtime: runtime, client: activeClient)
        } catch {
            lastError = Self.safeErrorMessage(error)
        }
    }

    func dismissLogin() {
        suppressCompletedLogin = true
        login = nil
    }
}
