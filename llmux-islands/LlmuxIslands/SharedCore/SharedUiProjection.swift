import Foundation

/// Native macOS projection of canonical Rust state. No daemon DTO is used as
/// an alternate source of display truth: every value below comes from
/// `SharedUiState` after privacy and receipt normalization in Rust.
extension SharedUiState {
    var connectionState: IslandUsageModel.Connection {
        switch lifecycle {
        case "ready": .online
        case "offline", "fatal": .offline(connection.error ?? "llmux not reachable")
        default: .connecting
        }
    }

    var dashboardProjection: LlmuxDashboard {
        let nowMs = connection.lastSuccessMs ?? UInt64(Date().timeIntervalSince1970 * 1_000)
        let healthByID = Dictionary(uniqueKeysWithValues: statistics.health.map { ($0.id, $0) })
        let accounts = usage.accounts.map { account in
            let health = healthByID[account.id]
            return LlmuxDashboardAccount(
                name: account.displayName,
                type: health?.kind ?? account.provider.daemonKind,
                status: account.status,
                blocked: account.blockedReason,
                healthy: account.healthy,
                fiveHour: account.gauge(.fiveHour)?.dashboardWindow(nowMs: nowMs),
                sevenDay: account.gauge(.sevenDay)?.dashboardWindow(nowMs: nowMs),
                fableWeekly: account.gauge(.fableWeekly)?.scopedWindow(nowMs: nowMs),
                cooldownUntil: health?.cooldownUntilMs.map { $0 / 1_000 },
                cooldownSource: health?.cooldownSource,
                inFlight: Int(account.inFlight),
                tokenExpiresAtMs: account.tokenExpiry?.expiresAtMs,
                lastRefreshMs: health?.lastRefreshMs,
                paused: account.paused
            )
        }
        let activity = LlmuxDashboardActivity(
            inFlight: statistics.activityReceipts.compactMap(\.inFlightProjection),
            completed: statistics.activityReceipts.compactMap(\.completedProjection)
        )
        return LlmuxDashboard(
            version: connection.daemonVersion ?? "llmux",
            port: URLComponents(string: connection.endpointDisplay)?.port ?? 3456,
            uptimeSecs: 0,
            current: usage.currentByGroup["claude"] ?? usage.currentByGroup.values.first,
            currentByGroup: usage.currentByGroup,
            accounts: accounts,
            totals: statistics.overview.dashboardProjection,
            modelUsage: statistics.models.map(\.dashboardProjection),
            clientUsage: statistics.clients.map(\.dashboardProjection),
            windowed: statistics.heatmaps.map(\.dashboardProjection),
            activity: activity,
            emailAnonymous: settings.emailAnonymous,
            showFableWeekly: settings.showFableWeekly,
            dataQuality: statistics.dataQuality.dashboardProjection,
            events: settings.events.map(\.dashboardProjection)
        )
    }

    var healthWarningCount: Int {
        usage.accounts.reduce(into: 0) { result, account in
            let overQuota = account.gauges.contains {
                $0.available && $0.usedFraction > DashboardHealth.quotaThreshold &&
                    ($0.kind != .fableWeekly || $0.constraining)
            }
            if account.status == "auth_failed" || overQuota { result += 1 }
        }
    }
}

extension SharedAccountTile {
    var usageTile: UsageAccountTile {
        let five = gauge(.fiveHour)
        let seven = gauge(.sevenDay)
        let fable = gauge(.fableWeekly)
        let email = displayName.contains("@") ? displayName : nil
        return UsageAccountTile(
            id: id,
            provider: provider.usageProvider,
            accountId: id,
            label: displayName,
            email: email,
            tier: nil,
            claudeIsTeam: nil,
            tokenRefresh: tokenExpiry.map {
                TokenRefreshInfo(
                    expiresAt: Date(timeIntervalSince1970: Double($0.expiresAtMs) / 1_000),
                    lifetimeSeconds: 8 * 3_600
                )
            },
            info: CLIUsageInfo(
                name: displayName,
                available: healthy,
                error: !healthy,
                fiveHourPercent: five.map { $0.usedFraction * 100 },
                sevenDayPercent: seven.map { $0.usedFraction * 100 },
                fiveHourReset: five?.resetDate,
                sevenDayReset: seven?.resetDate,
                model: nil,
                plan: nil,
                buckets: nil,
                fableWeeklyPercent: fable.map { $0.usedFraction * 100 },
                fableWeeklyReset: fable?.resetDate,
                fableWeeklySeverity: warningLevel,
                fableWeeklyIsActive: fable != nil,
                fableWeeklyConstraining: fable?.constraining
            ),
            errorMessage: healthy ? nil : (blockedReason ?? status),
            issue: nil,
            current: current,
            paused: paused,
            healthy: healthy,
            status: status,
            inFlight: Int(inFlight)
        )
    }
}

extension SharedProvider {
    var usageProvider: UsageProvider {
        switch self {
        case .codex: .codex
        case .grok: .grok
        case .claude, .api, .unknown: .claude
        }
    }

    var daemonKind: String {
        switch self {
        case .claude: "oauth"
        case .codex: "codex"
        case .grok: "grok"
        case .api: "apikey"
        case .unknown: "unknown"
        }
    }
}

extension SharedGauge {
    var resetDate: Date? { resetsAt.map { Date(timeIntervalSince1970: Double($0) / 1_000) } }

    func dashboardWindow(nowMs: UInt64) -> LlmuxDashboardWindow? {
        guard available else { return nil }
        return LlmuxDashboardWindow(
            utilization: usedFraction,
            resetsAt: resetsAt.map { $0 / 1_000 },
            resetsInSecs: resetsAt.map { Int($0.saturatingSubtract(nowMs) / 1_000) },
            fetchedAtMs: nil,
            source: nil
        )
    }

    func scopedWindow(nowMs: UInt64) -> LlmuxScopedWindow? {
        guard available else { return nil }
        return LlmuxScopedWindow(
            utilization: usedFraction,
            resetsInSecs: resetsAt.map { Int($0.saturatingSubtract(nowMs) / 1_000) },
            resetsAt: resetsAt.map { Int($0 / 1_000) },
            severity: constraining ? "critical" : "ok",
            isActive: true,
            constraining: constraining
        )
    }
}

extension SharedOverview {
    var dashboardProjection: LlmuxDashboardTotals {
        LlmuxDashboardTotals(
            requests: requests, ok: ok, errors: errors,
            tokensIn: tokensIn, tokensOut: tokensOut,
            rpm5m: rpm5m, inFlight: inFlight, costUsd: costUsd
        )
    }
}

extension SharedModelStatistics {
    var dashboardProjection: LlmuxDashboardModelUsage {
        LlmuxDashboardModelUsage(
            group: group, model: model, requests: requests, ok: ok, errors: errors,
            tokensIn: tokensIn, tokensOut: tokensOut, cacheRead: cacheRead,
            cacheCreation: cacheCreation, lastUsedMs: lastUsedMs, inFlight: inFlight,
            accounts: accounts.map(\.dashboardProjection),
            efforts: efforts.map(\.dashboardProjection),
            endpoints: endpoints.map(\.dashboardProjection), costUsd: costUsd
        )
    }
}

extension SharedStatisticsCount {
    var dashboardProjection: LlmuxDashboardModelCount {
        LlmuxDashboardModelCount(label: label, requests: requests)
    }
}

extension SharedModelAccountStatistics {
    var dashboardProjection: LlmuxDashboardModelAccount {
        LlmuxDashboardModelAccount(
            name: displayName, requests: requests, ok: ok, errors: errors,
            tokensIn: tokensIn, tokensOut: tokensOut
        )
    }
}

extension SharedClientStatistics {
    var dashboardProjection: LlmuxDashboardClientUsage {
        LlmuxDashboardClientUsage(
            client: client, requests: requests, ok: ok, errors: errors,
            tokensIn: tokensIn, tokensOut: tokensOut,
            costUsd: costUsd, lastSeenMs: lastSeenMs
        )
    }
}

extension SharedHeatmapStatistics {
    var dashboardProjection: LlmuxDashboardWindowed {
        LlmuxDashboardWindowed(window: window, windowSecs: windowSecs, cells: cells.map(\.dashboardProjection))
    }
}

extension SharedHeatmapCell {
    var dashboardProjection: LlmuxDashboardWindowedCell {
        LlmuxDashboardWindowedCell(
            group: group, model: model, account: accountDisplay,
            requests: requests, ok: ok, errors: errors,
            tokensIn: tokensIn, tokensOut: tokensOut,
            cacheRead: cacheRead, cacheCreation: cacheCreation, tokens: tokens
        )
    }
}

extension SharedActivityReceipt {
    var inFlightProjection: LlmuxDashboardInFlight? {
        guard kind == "in_flight" else { return nil }
        let suffix = receiptId.split(separator: ":").last.flatMap { UInt64($0) }
        return LlmuxDashboardInFlight(
            id: suffix ?? Self.stableIdentifier(receiptId),
            method: method ?? "", path: path ?? "", account: accountDisplay,
            startedAtMs: occurredAtMs, group: provider?.rawValue, model: model
        )
    }

    var completedProjection: LlmuxDashboardCompleted? {
        guard kind != "in_flight" else { return nil }
        if kind == "note" {
            return LlmuxDashboardCompleted(
                kind: "note", atMs: occurredAtMs,
                method: nil, path: nil, account: nil, status: nil, durationMs: nil,
                tokens: nil, costUsd: nil, group: nil, model: nil, effort: nil,
                text: message, error: error
            )
        }
        return LlmuxDashboardCompleted(
            kind: "request", atMs: occurredAtMs,
            method: method, path: path, account: accountDisplay, status: status,
            durationMs: durationMs,
            tokens: tokens.map { .init(input: $0.input, output: $0.output) },
            costUsd: costUsd, group: provider?.rawValue, model: model, effort: effort,
            text: nil, error: error
        )
    }

    private static func stableIdentifier(_ value: String) -> UInt64 {
        value.utf8.reduce(14_695_981_039_346_656_037) { ($0 ^ UInt64($1)) &* 1_099_511_628_211 }
    }
}

extension SharedDataQuality {
    var dashboardProjection: LlmuxDashboardDataQuality {
        LlmuxDashboardDataQuality(modelUsage: modelUsage, windowed: windowed, cost: cost, cache: cache)
    }
}

extension SharedEvent {
    var dashboardProjection: LlmuxEvent {
        LlmuxEvent(id: id, from: from, to: to, content: content)
    }
}

private extension UInt64 {
    func saturatingSubtract(_ other: UInt64) -> UInt64 { self >= other ? self - other : 0 }
}

// MARK: - Initializers for native view DTO projections

extension LlmuxDashboard {
    init(
        version: String, port: Int, uptimeSecs: UInt64, current: String?,
        currentByGroup: [String: String], accounts: [LlmuxDashboardAccount],
        totals: LlmuxDashboardTotals, modelUsage: [LlmuxDashboardModelUsage],
        clientUsage: [LlmuxDashboardClientUsage], windowed: [LlmuxDashboardWindowed],
        activity: LlmuxDashboardActivity, emailAnonymous: Bool?, showFableWeekly: Bool?,
        dataQuality: LlmuxDashboardDataQuality?, events: [LlmuxEvent]
    ) {
        self.version = version; self.port = port; self.uptimeSecs = uptimeSecs
        self.current = current; self.currentByGroup = currentByGroup; self.accounts = accounts
        self.totals = totals; self.modelUsage = modelUsage; self.clientUsage = clientUsage
        self.windowed = windowed; self.activity = activity; self.emailAnonymous = emailAnonymous
        self.showFableWeekly = showFableWeekly; self.dataQuality = dataQuality; self.events = events
    }
}

extension LlmuxDashboardModelUsage {
    init(
        group: String, model: String, requests: UInt64, ok: UInt64, errors: UInt64,
        tokensIn: UInt64, tokensOut: UInt64, cacheRead: UInt64?, cacheCreation: UInt64?,
        lastUsedMs: UInt64, inFlight: Int, accounts: [LlmuxDashboardModelAccount],
        efforts: [LlmuxDashboardModelCount], endpoints: [LlmuxDashboardModelCount], costUsd: Double?
    ) {
        self.group = group; self.model = model; self.requests = requests; self.ok = ok
        self.errors = errors; self.tokensIn = tokensIn; self.tokensOut = tokensOut
        self.cacheRead = cacheRead; self.cacheCreation = cacheCreation; self.lastUsedMs = lastUsedMs
        self.inFlight = inFlight; self.accounts = accounts; self.efforts = efforts
        self.endpoints = endpoints; self.costUsd = costUsd
    }
}

extension LlmuxDashboardWindowedCell {
    init(
        group: String, model: String, account: String,
        requests: UInt64, ok: UInt64, errors: UInt64,
        tokensIn: UInt64, tokensOut: UInt64,
        cacheRead: UInt64, cacheCreation: UInt64, tokens: UInt64
    ) {
        self.group = group; self.model = model; self.account = account
        self.requests = requests; self.ok = ok; self.errors = errors
        self.tokensIn = tokensIn; self.tokensOut = tokensOut
        self.cacheRead = cacheRead; self.cacheCreation = cacheCreation; self.tokens = tokens
    }
}
