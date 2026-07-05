import SwiftUI

// Islands analytics UI over the `/llmux/dashboard` contract (issue #62 S4,
// gist §4.5/§4.6): summary cards, top models, heat strip, recent activity,
// health banner, and the Overview / Models / Clients / Health tab structure.
// Rendered ONLY when a dashboard document is present — the `/llmux/status`
// fallback path (old daemons) keeps the plain tile grid.
//
// Detail views are in-panel expansions, not sheets: sheets are unreliable in
// the borderless island (see the AddAccountInline note in IslandUsageView).

/// The tabbed analytics section embedded in `IslandUsageView`. `accountTiles`
/// injects the existing tile grid into the Overview tab so tile behavior is
/// preserved verbatim (U4).
struct UsageAnalyticsSection<AccountTiles: View>: View {
    @ObservedObject var model: IslandUsageModel
    let dashboard: LlmuxDashboard
    let now: Date
    @ViewBuilder let accountTiles: () -> AccountTiles

    @State private var tab: Tab
    @State private var expandedModelID: String?
    @State private var expandedAccount: String?

    enum Tab: String, CaseIterable {
        case overview = "Overview"
        case models = "Models"
        case clients = "Clients"
        case health = "Health"
    }

    init(
        model: IslandUsageModel,
        dashboard: LlmuxDashboard,
        now: Date,
        initialTab: Tab = .overview,
        @ViewBuilder accountTiles: @escaping () -> AccountTiles
    ) {
        self.model = model
        self.dashboard = dashboard
        self.now = now
        self.accountTiles = accountTiles
        _tab = State(initialValue: initialTab)
    }

    private var labels: DataQualityLabels { DataQualityLabels(dashboard.dataQuality) }
    private var health: DashboardHealth.Summary { DashboardHealth.summary(dashboard.accounts) }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            if health.isWarning {
                UsageHealthBanner(summary: health)   // U11 — same rule as the closed-island color
            }

            Picker("", selection: $tab) {
                ForEach(Tab.allCases, id: \.self) { Text($0.rawValue).tag($0) }
            }
            .pickerStyle(.segmented)
            .labelsHidden()

            switch tab {
            case .overview: overview
            case .models: modelList(rows: dashboard.modelUsage, caption: labels.modelUsage)
            case .clients: clientsTab
            case .health: healthTab
            }
        }
    }

    // MARK: - Overview (gist §4.5: cards, tiles, top models, heat, activity)

    @ViewBuilder private var overview: some View {
        UsageSummaryCards(totals: dashboard.totals, costCaption: labels.cost)   // U7
        accountTiles()                                                          // U4
        if !dashboard.modelUsage.isEmpty {                                      // U8
            modelList(
                rows: LlmuxDashboardModelUsage.top(dashboard.modelUsage),
                caption: "top models — \(labels.modelUsage)"
            )
        }
        if !dashboard.windowed.isEmpty {                                        // U9
            UsageHeatStrip(windowed: dashboard.windowed, caption: labels.windowed)
        }
        UsageActivityList(activity: dashboard.activity, now: now)               // U10
    }

    // MARK: - Models

    private func modelList(rows: [LlmuxDashboardModelUsage], caption: String) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            // ForEach keys off `row.id` = (group, model) — never the model text.
            ForEach(rows) { row in
                UsageModelRow(
                    row: row,
                    isExpanded: expandedModelID == row.id,
                    cacheCaption: labels.cache,
                    now: now
                ) {
                    withAnimation(.easeInOut(duration: 0.15)) {
                        expandedModelID = expandedModelID == row.id ? nil : row.id
                    }
                }
            }
            if rows.isEmpty {
                AnalyticsCaption(text: "no model usage yet")
            }
            AnalyticsCaption(text: caption)   // U22 scope label
        }
    }

    // MARK: - Clients

    @ViewBuilder private var clientsTab: some View {
        VStack(alignment: .leading, spacing: 6) {
            ForEach(dashboard.clientUsage) { client in
                HStack(spacing: 8) {
                    // Raw wire id (may be a metadata.user_id JSON blob) →
                    // short human label; non-JSON ids render as-is (#68).
                    Text(ClientIDLabel.display(client.client))
                        .font(.system(size: 11, weight: .semibold, design: .monospaced))
                        .foregroundColor(.white.opacity(0.85))
                        .lineLimit(1)
                        .truncationMode(.middle)
                    Spacer()
                    MetricText("\(DashFormat.count(client.requests)) req")
                    MetricText("\(DashFormat.count(client.tokensIn &+ client.tokensOut)) tok")
                    MetricText("\(DashFormat.count(client.errors)) err")
                    // #32 out of scope: cost + last-seen are wire-ready zeros
                    // today — absent data is an omitted element, never a `—`
                    // placeholder column (#68).
                    if let cost = DashFormat.clientCost(client.costUsd) {
                        MetricText(cost)
                    }
                    if let seen = DashFormat.clientLastSeen(client.lastSeenMs, now: now) {
                        MetricText(seen)
                    }
                }
                .padding(8)
                .background(RoundedRectangle(cornerRadius: 8).fill(Color.white.opacity(0.05)))
            }
            if dashboard.clientUsage.isEmpty {
                AnalyticsCaption(text: "no client data")
            }
        }
    }

    // MARK: - Health (account detail = in-panel expansion, U12)

    @ViewBuilder private var healthTab: some View {
        VStack(alignment: .leading, spacing: 6) {
            ForEach(dashboard.accounts, id: \.name) { account in
                UsageAccountHealthRow(
                    account: account,
                    isExpanded: expandedAccount == account.name,
                    now: now
                ) {
                    withAnimation(.easeInOut(duration: 0.15)) {
                        expandedAccount = expandedAccount == account.name ? nil : account.name
                    }
                }
            }
        }
    }
}

// MARK: - Summary cards (U7)

struct UsageSummaryCards: View {
    let totals: LlmuxDashboardTotals
    /// `data_quality.cost` or its byte-identical fallback (U20 — rendered).
    let costCaption: String

    var body: some View {
        HStack(spacing: 8) {
            card("Requests", DashFormat.count(totals.requests), nil)
            card("Tokens", DashFormat.count(totals.totalTokens), nil)
            // nil cost (old daemon) renders `—`, never $0.00.
            card("Cost", DashFormat.cost(totals.costUsd), costCaption)
            card(
                "Errors",
                DashFormat.percent(DashFormat.errorRate(errors: totals.errors, requests: totals.requests)),
                "\(DashFormat.count(totals.errors)) errors"
            )
        }
    }

    private func card(_ title: String, _ value: String, _ caption: String?) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(title.uppercased())
                .font(.system(size: 8, weight: .semibold, design: .monospaced))
                .foregroundColor(.white.opacity(0.4))
            Text(value)
                .font(.system(size: 15, weight: .semibold, design: .monospaced))
                .foregroundColor(.white.opacity(0.9))
                .lineLimit(1)
                .minimumScaleFactor(0.6)
            if let caption {
                Text(caption)
                    .font(.system(size: 8))
                    .foregroundColor(.white.opacity(0.35))
                    .lineLimit(1)
                    .minimumScaleFactor(0.7)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(8)
        .background(RoundedRectangle(cornerRadius: 8).fill(Color.white.opacity(0.05)))
    }
}

// MARK: - Health banner (U11)

struct UsageHealthBanner: View {
    let summary: DashboardHealth.Summary

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: "exclamationmark.triangle.fill")
                .font(.system(size: 11))
                .foregroundColor(TerminalColors.amber)
            Text(DashboardHealth.bannerText(summary))
                .font(.system(size: 11, weight: .semibold))
                .foregroundColor(.white.opacity(0.9))
                .lineLimit(1)
                .minimumScaleFactor(0.8)
            Spacer()
        }
        .padding(8)
        .background(RoundedRectangle(cornerRadius: 8).fill(TerminalColors.amber.opacity(0.15)))
        .overlay(RoundedRectangle(cornerRadius: 8).stroke(TerminalColors.amber.opacity(0.4), lineWidth: 1))
    }
}

// MARK: - Model row + in-panel detail (U8 / U12)

struct UsageModelRow: View {
    let row: LlmuxDashboardModelUsage
    let isExpanded: Bool
    /// `data_quality.cache` or fallback — shown where cache counters render (U21).
    let cacheCaption: String
    let now: Date
    let onTap: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Button(action: onTap) { header.contentShape(Rectangle()) }
                .buttonStyle(.plain)
            if isExpanded { detail }
        }
        .padding(8)
        .background(RoundedRectangle(cornerRadius: 8).fill(Color.white.opacity(isExpanded ? 0.08 : 0.05)))
    }

    private var header: some View {
        HStack(spacing: 8) {
            GroupBadge(group: row.group)
            Text(row.model)
                .font(.system(size: 11, weight: .semibold, design: .monospaced))
                .foregroundColor(.white.opacity(0.85))
                .lineLimit(1)
                .truncationMode(.middle)
            Spacer()
            MetricText("\(DashFormat.count(row.totalTokens)) tok")
            MetricText("\(DashFormat.count(row.requests)) req")
            // Absent cost (old daemon / no pricing) = omitted, not `—` (#68).
            if let costUsd = row.costUsd {
                MetricText(DashFormat.cost(costUsd))
            }
            Image(systemName: isExpanded ? "chevron.up" : "chevron.down")
                .font(.system(size: 8, weight: .semibold))
                .foregroundColor(.white.opacity(0.35))
        }
    }

    private var detail: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 14) {
                StatText(label: "ok", value: DashFormat.count(row.ok))
                StatText(label: "errors", value: DashFormat.count(row.errors))
                StatText(label: "in-flight", value: "\(row.inFlight)")
                StatText(label: "last", value: DashFormat.ago(ms: row.lastUsedMs, now: now))
            }
            // nil cache counters are NEVER shown as a fabricated 0 (U21); a
            // fully absent pair omits the row instead of dashes (#68).
            if row.cacheRead != nil || row.cacheCreation != nil {
                HStack(spacing: 14) {
                    if let cacheRead = row.cacheRead {
                        StatText(label: "cache read", value: DashFormat.count(cacheRead))
                    }
                    if let cacheCreation = row.cacheCreation {
                        StatText(label: "cache create", value: DashFormat.count(cacheCreation))
                    }
                }
            }
            ForEach(row.accounts.prefix(3), id: \.name) { account in
                HStack(spacing: 8) {
                    Text(account.name)
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundColor(.white.opacity(0.5))
                        .lineLimit(1)
                        .truncationMode(.middle)
                    Spacer()
                    MetricText("\(DashFormat.count(account.requests)) req")
                    MetricText("\(DashFormat.count(account.tokensIn &+ account.tokensOut)) tok")
                }
            }
            AnalyticsCaption(text: cacheCaption)
        }
        .padding(.top, 2)
    }
}

// MARK: - Heat strip (U9)

struct UsageHeatStrip: View {
    let windowed: [LlmuxDashboardWindowed]
    /// `data_quality.windowed` or fallback ("best effort", U19).
    let caption: String

    private let maxCells = 24

    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            ForEach(windowed, id: \.window) { slice in
                let cells = Array(slice.cells.sorted { $0.tokens > $1.tokens }.prefix(maxCells))
                let peak = cells.map(\.tokens).max() ?? 0
                HStack(spacing: 6) {
                    Text(slice.window)
                        .font(.system(size: 10, weight: .semibold, design: .monospaced))
                        .foregroundColor(.white.opacity(0.45))
                        .frame(width: 28, alignment: .leading)
                    HStack(spacing: 2) {
                        ForEach(cells) { cell in
                            RoundedRectangle(cornerRadius: 2)
                                .fill(color(for: cell, peak: peak))
                                .frame(width: 10, height: 10)
                                .help("\(cell.group)/\(cell.model) — \(cell.account): \(DashFormat.count(cell.tokens)) tok, \(DashFormat.count(cell.requests)) req")
                        }
                    }
                    Spacer(minLength: 0)
                }
            }
            AnalyticsCaption(text: caption)
        }
    }

    private func color(for cell: LlmuxDashboardWindowedCell, peak: UInt64) -> Color {
        let base = cell.group == "codex" ? TerminalColors.blue : TerminalColors.amber
        let intensity = peak > 0 ? Double(cell.tokens) / Double(peak) : 0
        return base.opacity(0.2 + 0.8 * intensity)
    }
}

// MARK: - Recent activity (U10 — metadata ONLY, never prompt/response content)

struct UsageActivityList: View {
    let activity: LlmuxDashboardActivity
    let now: Date

    private let completedLimit = 8
    private let inFlightLimit = 3

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            ForEach(activity.inFlight.prefix(inFlightLimit), id: \.id) { flight in
                row(
                    time: DashFormat.ago(ms: flight.startedAtMs, now: now),
                    status: Text("···").foregroundColor(TerminalColors.amber),
                    label: flight.model ?? flight.path,
                    trailing: "in flight"
                )
            }
            ForEach(Array(activity.completed.prefix(completedLimit).enumerated()), id: \.offset) { _, entry in
                completedRow(entry)
            }
            if activity.inFlight.isEmpty, activity.completed.isEmpty {
                AnalyticsCaption(text: "no recent activity")
            }
        }
    }

    @ViewBuilder private func completedRow(_ entry: LlmuxDashboardCompleted) -> some View {
        if entry.isNote {
            // Operator note from the daemon ("token refreshed: …") — server
            // telemetry, not request content.
            HStack(spacing: 6) {
                timeText(DashFormat.ago(ms: entry.atMs, now: now))
                Text(entry.text ?? "note")
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundColor(entry.error == true ? TerminalColors.amber : .white.opacity(0.45))
                    .lineLimit(1)
                    .truncationMode(.tail)
                Spacer(minLength: 0)
            }
        } else {
            row(
                time: DashFormat.ago(ms: entry.atMs, now: now),
                status: statusText(entry.status),
                label: entry.model ?? entry.path ?? entry.method ?? "request",
                trailing: [
                    entry.tokens.map { "\(DashFormat.count($0.input))→\(DashFormat.count($0.output))" },
                    entry.costUsd.map { DashFormat.cost($0) },
                    entry.durationMs.map { DashFormat.duration(ms: $0) },
                ]
                .compactMap { $0 }
                .joined(separator: "  ")
            )
        }
    }

    private func row(time: String, status: Text, label: String, trailing: String) -> some View {
        HStack(spacing: 6) {
            timeText(time)
            status.font(.system(size: 9, weight: .semibold, design: .monospaced))
                .frame(width: 24, alignment: .leading)
            Text(label)
                .font(.system(size: 9, design: .monospaced))
                .foregroundColor(.white.opacity(0.7))
                .lineLimit(1)
                .truncationMode(.middle)
            Spacer(minLength: 6)
            Text(trailing)
                .font(.system(size: 9, design: .monospaced))
                .foregroundColor(.white.opacity(0.45))
                .lineLimit(1)
        }
    }

    private func timeText(_ value: String) -> some View {
        Text(value)
            .font(.system(size: 9, design: .monospaced))
            .foregroundColor(.white.opacity(0.35))
            .frame(width: 30, alignment: .trailing)
    }

    private func statusText(_ status: Int?) -> Text {
        guard let status else { return Text("—").foregroundColor(.white.opacity(0.35)) }
        let color: Color =
            status < 400 ? TerminalColors.green : status < 500 ? TerminalColors.amber : TerminalColors.red
        return Text("\(status)").foregroundColor(color)
    }
}

// MARK: - Account health row + in-panel detail (U12)

struct UsageAccountHealthRow: View {
    let account: LlmuxDashboardAccount
    let isExpanded: Bool
    let now: Date
    let onTap: () -> Void

    @AppStorage(AppSettings.emailAnonymousEnabledKey) private var emailAnonymousEnabled = false

    private var isAuthFailed: Bool { account.status == "auth_failed" }
    private var isOverQuota: Bool {
        DashboardHealth.isOverQuota(
            fiveHour: account.fiveHour?.utilization,
            sevenDay: account.sevenDay?.utilization,
            fableUtilization: account.fableWeekly?.utilization,
            fableConstraining: account.fableWeekly?.constraining
        )
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Button(action: onTap) { header.contentShape(Rectangle()) }
                .buttonStyle(.plain)
            if isExpanded { detail }
        }
        .padding(8)
        .background(RoundedRectangle(cornerRadius: 8).fill(Color.white.opacity(isExpanded ? 0.08 : 0.05)))
    }

    private var header: some View {
        HStack(spacing: 8) {
            Circle()
                .fill(isAuthFailed ? TerminalColors.red : isOverQuota ? TerminalColors.amber : TerminalColors.green)
                .frame(width: 6, height: 6)
            EmailPixelized(
                isActive: emailAnonymousEnabled && account.name.contains("@"),
                cacheKey: account.name
            ) {
                Text(account.name)
                    .font(.system(size: 11, weight: .semibold, design: .monospaced))
                    .foregroundColor(.white.opacity(0.85))
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
            Spacer()
            // Cold account (no windows reported) omits the summary — no `—`.
            if let quotaSummary {
                MetricText(quotaSummary)
            }
            if let inFlight = account.inFlight, inFlight > 0 {
                MetricText("▶\(inFlight)")
            }
            Image(systemName: isExpanded ? "chevron.up" : "chevron.down")
                .font(.system(size: 8, weight: .semibold))
                .foregroundColor(.white.opacity(0.35))
        }
    }

    /// nil (cold account, no windows reported) omits the element entirely (#68).
    private var quotaSummary: String? {
        var parts: [String] = []
        if let five = account.fiveHour { parts.append("5h \(Int((five.utilization * 100).rounded()))%") }
        if let seven = account.sevenDay { parts.append("7d \(Int((seven.utilization * 100).rounded()))%") }
        if let fable = account.fableWeekly { parts.append("Fab \(Int((fable.utilization * 100).rounded()))%") }
        return parts.isEmpty ? nil : parts.joined(separator: " · ")
    }

    private var detail: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 14) {
                if let status = account.status {
                    StatText(label: "status", value: status)
                }
                StatText(label: "type", value: account.type)
                if let cooldownSource = account.cooldownSource {
                    StatText(label: "cooldown", value: cooldownSource)
                }
            }
            if let blocked = account.blocked {
                StatText(label: "blocked", value: blocked)
            }
            // Absent instants are omitted — never a `—` stat (#68).
            if account.tokenExpiresAtMs != nil || account.lastRefreshMs != nil {
                HStack(spacing: 14) {
                    if let expires = account.tokenExpiresAtMs {
                        StatText(label: "token", value: DashFormat.until(ms: expires, now: now))
                    }
                    if let refreshed = account.lastRefreshMs {
                        StatText(label: "refreshed", value: DashFormat.ago(ms: refreshed, now: now))
                    }
                }
            }
        }
        .padding(.top, 2)
    }
}

// MARK: - Small shared pieces

struct GroupBadge: View {
    let group: String

    var body: some View {
        Text(group.uppercased())
            .font(.system(size: 8, weight: .bold, design: .monospaced))
            .foregroundColor(group == "codex" ? TerminalColors.blue : TerminalColors.amber)
            .padding(.horizontal, 5)
            .padding(.vertical, 2)
            .background(
                Capsule(style: .continuous)
                    .fill((group == "codex" ? TerminalColors.blue : TerminalColors.amber).opacity(0.15))
            )
    }
}

struct MetricText: View {
    let value: String

    init(_ value: String) { self.value = value }

    var body: some View {
        Text(value)
            .font(.system(size: 9, weight: .semibold, design: .monospaced))
            .foregroundColor(.white.opacity(0.5))
            .lineLimit(1)
    }
}

struct StatText: View {
    let label: String
    let value: String

    var body: some View {
        HStack(spacing: 4) {
            Text(label)
                .font(.system(size: 9, design: .monospaced))
                .foregroundColor(.white.opacity(0.35))
            Text(value)
                .font(.system(size: 9, weight: .semibold, design: .monospaced))
                .foregroundColor(.white.opacity(0.7))
        }
        .lineLimit(1)
    }
}

struct AnalyticsCaption: View {
    let text: String

    var body: some View {
        Text(text)
            .font(.system(size: 8))
            .foregroundColor(.white.opacity(0.35))
            .lineLimit(1)
    }
}
