import SwiftUI

// Statistics-panel building blocks (issue #68 v2) over the `/llmux/dashboard`
// contract (#62): summary cards, model rows, client rows, heat strip, recent
// activity and account health. All four sections live behind the ☰ menu's
// "Statistics" entry (`IslandStatsView`) — the default Usage panel is the
// v0.2.14 account-tile view and renders none of this.
//
// Design language (extracted from the v0.2.14 pill/tiles/menu):
// - near-black panel, cards = `Color.white.opacity(0.05)` rounded 8 — the ONE
//   card fill every row/card here shares;
// - amber accent (`TerminalColors.amber`, the pill's warning/accent hue) for
//   selection + claude group marks; codex keeps the tiles' blue;
// - monospaced digits, numeric columns right-aligned on a fixed grid;
// - small-caps monospaced secondary section labels (`StatsSectionLabel`);
// - red ONLY for genuine warn states (≥ 90% quota, auth_failed, 5xx);
// - absent data is an OMITTED element — never a `—` placeholder (#68).
//
// Detail views are in-panel expansions, not sheets: sheets are unreliable in
// the borderless island (see the AddAccountInline note in IslandUsageView).

// MARK: - Section switcher

/// Flat monochrome local switcher. Frequent navigation is immediate and does
/// not dispatch a shared semantic action.
struct StatsSegmentedControl: View {
    @Binding var selection: StatsSection

    var body: some View {
        HStack(spacing: 3) {
            ForEach(StatsSection.allCases, id: \.self) { section in
                segment(section)
            }
        }
        .overlay(alignment: .bottom) {
            Rectangle().fill(Color.white.opacity(0.12)).frame(height: 1)
        }
    }

    private func segment(_ section: StatsSection) -> some View {
        let selected = selection == section
        return Button {
            selection = section
        } label: {
            Text(section.title)
                .font(.caption.weight(selected ? .semibold : .regular))
                .foregroundColor(selected ? .black : .white.opacity(0.6))
                .frame(maxWidth: .infinity)
                .frame(minHeight: 44)
                .background(selected ? Color.white : Color.clear)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }
}

struct StatsSectionLabel: View {
    let text: String

    init(_ text: String) { self.text = text }

    var body: some View {
        Text(text)
            .font(.caption.weight(.semibold))
            .foregroundColor(.white.opacity(0.6))
    }
}

// MARK: - Section content

/// One Statistics section, dispatched by `StatsSection`. Shared by the live
/// panel (`IslandStatsView`) and snapshot mode, so the PNGs show exactly what
/// the app renders.
struct StatsSectionContent: View {
    let section: StatsSection
    let dashboard: LlmuxDashboard
    /// `/llmux/status`-shaped account tiles — the Overview's compact account
    /// rows read the same source as the Usage panel's tile grid.
    let tiles: [UsageAccountTile]
    let now: Date
    var activityReceipts: [SharedActivityReceipt] = []
    var verificationReceipts: [SharedVerificationReceipt] = []
    var onRemoveAccount: ((String) -> Void)? = nil
    var includePrimaryOverview = true

    @State private var expandedModelID: String?
    @State private var expandedAccount: String?

    private var labels: DataQualityLabels { DataQualityLabels(dashboard.dataQuality) }
    private var health: DashboardHealth.Summary { DashboardHealth.summary(dashboard.accounts) }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            switch section {
            case .overview: overview
            case .models: models
            case .clients: clients
            case .health: healthSection
            }
        }
    }

    // MARK: Overview — totals, accounts, top models, heat, activity

    @ViewBuilder private var overview: some View {
        if includePrimaryOverview {
            if health.isWarning {
                UsageHealthBanner(summary: health)   // U11 — same rule as the closed pill
            }
            UsageSummaryCards(totals: dashboard.totals, costCaption: labels.cost)
            if !tiles.isEmpty {
                block("Accounts") {
                    UsageAccountCompactList(tiles: tiles, onRemove: onRemoveAccount)
                }
            }
        }
        if !dashboard.modelUsage.isEmpty {
            block("Top models") {
                modelList(rows: LlmuxDashboardModelUsage.top(dashboard.modelUsage), caption: labels.modelUsage)
            }
        }
        if !dashboard.windowed.isEmpty {
            block("Token heat") {
                UsageHeatStrip(windowed: dashboard.windowed, caption: labels.windowed)
            }
        }
        block("Recent activity") {
            if activityReceipts.isEmpty {
                UsageActivityList(activity: dashboard.activity, now: now)
            } else {
                UsageCanonicalActivityReceiptList(receipts: activityReceipts, now: now)
            }
        }
        if !verificationReceipts.isEmpty {
            block("Verification receipts") {
                UsageVerificationReceiptList(receipts: verificationReceipts, now: now)
            }
        }
    }

    // MARK: Models

    @ViewBuilder private var models: some View {
        block("models") {
            modelList(rows: dashboard.modelUsage, caption: labels.modelUsage)
        }
    }

    private func modelList(rows: [LlmuxDashboardModelUsage], caption: String) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            // ForEach keys off `row.id` = (group, model) — never the model text.
            ForEach(rows) { row in
                UsageModelRow(
                    row: row,
                    isExpanded: expandedModelID == row.id,
                    cacheCaption: labels.cache,
                    now: now
                ) {
                    expandedModelID = expandedModelID == row.id ? nil : row.id
                }
            }
            if rows.isEmpty {
                AnalyticsCaption(text: "no model usage yet")
            }
            AnalyticsCaption(text: caption)   // U22 scope label
        }
    }

    // MARK: Clients

    @ViewBuilder private var clients: some View {
        block("clients") {
            VStack(alignment: .leading, spacing: 4) {
                ForEach(dashboard.clientUsage) { client in
                    UsageClientRow(client: client, now: now)
                }
                if dashboard.clientUsage.isEmpty {
                    AnalyticsCaption(text: "no client data")
                }
            }
        }
    }

    // MARK: Health (account detail = in-panel expansion, U12)

    @ViewBuilder private var healthSection: some View {
        block("account health") {
            VStack(alignment: .leading, spacing: 4) {
                ForEach(Array(dashboard.accounts.enumerated()), id: \.element.name) { index, account in
                    UsageAccountHealthRow(
                        account: account,
                        isExpanded: expandedAccount == account.name,
                        now: now,
                        privacyAlias: IslandPresentationPolicy.privateAccountLabel(
                            providerName: providerName(for: account),
                            ordinal: index + 1
                        )
                    ) {
                        expandedAccount = expandedAccount == account.name ? nil : account.name
                    }
                }
            }
        }
    }

    private func block(_ label: String, @ViewBuilder content: () -> some View) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            StatsSectionLabel(label)
            content()
        }
    }

    private func providerName(for account: LlmuxDashboardAccount) -> String {
        switch account.type.lowercased() {
        case "codex": "Codex"
        case "grok": "Grok"
        default: "Claude"
        }
    }
}

/// Rust-owned activity receipts. Paths, account labels and notes have already
/// passed the shared privacy filter before reaching this renderer.
struct UsageCanonicalActivityReceiptList: View {
    let receipts: [SharedActivityReceipt]
    let now: Date

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            ForEach(receipts.prefix(11)) { receipt in
                HStack(spacing: 6) {
                    Text(DashFormat.ago(ms: receipt.occurredAtMs, now: now))
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundColor(.white.opacity(0.5))
                        .frame(width: 30, alignment: .trailing)
                    Text(status(receipt))
                        .font(.system(size: 9, weight: .semibold, design: .monospaced))
                        .foregroundColor(statusColor(receipt))
                        .frame(width: 28, alignment: .leading)
                    Text(receipt.message ?? receipt.model ?? receipt.path ?? receipt.method ?? receipt.kind)
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundColor(.white.opacity(0.7))
                        .lineLimit(1)
                        .truncationMode(.middle)
                    Spacer(minLength: 6)
                    Text(trailing(receipt))
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundColor(.white.opacity(0.6))
                        .lineLimit(1)
                }
            }
        }
        .padding(8)
        .background(Color.white.opacity(0.04))
    }

    private func status(_ receipt: SharedActivityReceipt) -> String {
        if receipt.kind == "in_flight" { return "···" }
        if receipt.kind == "note" { return "note" }
        return receipt.status.map(String.init) ?? ""
    }

    private func statusColor(_ receipt: SharedActivityReceipt) -> Color {
        if receipt.kind == "in_flight" { return TerminalColors.amber }
        guard let status = receipt.status else {
            return receipt.error ? TerminalColors.red : .white.opacity(0.6)
        }
        return status < 400 ? TerminalColors.green : status < 500 ? TerminalColors.amber : TerminalColors.red
    }

    private func trailing(_ receipt: SharedActivityReceipt) -> String {
        if receipt.kind == "in_flight" {
            return receipt.elapsedMs.map { DashFormat.duration(ms: $0) } ?? "in flight"
        }
        return [
            receipt.tokens.map { "\(DashFormat.count($0.input))→\(DashFormat.count($0.output))" },
            receipt.costUsd.map { DashFormat.cost($0) },
            receipt.durationMs.map { DashFormat.duration(ms: $0) },
        ].compactMap { $0 }.joined(separator: "  ")
    }
}

/// Bounded operation evidence emitted by the shared reducer after a daemon or
/// platform mutation finishes. This makes success/failure/no-change visible
/// without exposing executor-only raw account identifiers.
struct UsageVerificationReceiptList: View {
    let receipts: [SharedVerificationReceipt]
    let now: Date

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            ForEach(receipts.prefix(8)) { receipt in
                HStack(spacing: 6) {
                    Text(DashFormat.ago(ms: receipt.finishedAtMs, now: now))
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundColor(.white.opacity(0.5))
                        .frame(width: 30, alignment: .trailing)
                    Image(systemName: outcomeSymbol(receipt.outcome))
                        .font(.system(size: 9, weight: .semibold))
                        .foregroundColor(outcomeColor(receipt.outcome))
                        .frame(width: 12)
                    Text(receipt.operation.replacingOccurrences(of: "_", with: " "))
                        .font(.system(size: 9, weight: .semibold, design: .monospaced))
                        .foregroundColor(.white.opacity(0.75))
                    if let target = receipt.targetDisplay {
                        Text(target)
                            .font(.system(size: 9, design: .monospaced))
                            .foregroundColor(.white.opacity(0.5))
                            .lineLimit(1)
                            .truncationMode(.middle)
                    }
                    Spacer(minLength: 4)
                    Text(receipt.message)
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundColor(.white.opacity(0.6))
                        .lineLimit(1)
                }
            }
        }
        .padding(8)
        .background(Color.white.opacity(0.04))
    }

    private func outcomeSymbol(_ outcome: String) -> String {
        switch outcome {
        case "succeeded": "checkmark.circle.fill"
        case "no_change": "minus.circle.fill"
        case "cancelled": "xmark.circle"
        default: "exclamationmark.triangle.fill"
        }
    }

    private func outcomeColor(_ outcome: String) -> Color {
        switch outcome {
        case "succeeded": TerminalColors.green
        case "no_change", "cancelled": TerminalColors.amber
        default: TerminalColors.red
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
            Text(title)
                .font(.caption2.weight(.semibold))
                .foregroundColor(.white.opacity(0.6))
            Text(value)
                .font(.system(size: 15, weight: .semibold, design: .monospaced))
                .foregroundColor(.white.opacity(0.9))
                .lineLimit(1)
                .minimumScaleFactor(0.6)
            if let caption {
                Text(caption)
                    .font(.system(size: 8))
                    .foregroundColor(.white.opacity(0.5))
                    .lineLimit(1)
                    .minimumScaleFactor(0.7)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(8)
        .background(Color.white.opacity(0.04))
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
        .background(Color.white.opacity(0.04))
        .overlay(alignment: .leading) {
            Rectangle().fill(TerminalColors.amber).frame(width: 2)
        }
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
        .background(Color.white.opacity(isExpanded ? 0.08 : 0.04))
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
            MetricText("\(DashFormat.count(row.totalTokens)) tok", width: 58)
            MetricText("\(DashFormat.count(row.requests)) req", width: 54)
            // Absent cost (old daemon / no pricing) = omitted, not `—` (#68).
            if let costUsd = row.costUsd {
                MetricText(DashFormat.cost(costUsd), width: 44)
            }
            Image(systemName: isExpanded ? "chevron.up" : "chevron.down")
                .font(.system(size: 8, weight: .semibold))
                .foregroundColor(.white.opacity(0.5))
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
                    MetricText("\(DashFormat.count(account.requests)) req", width: 54)
                    MetricText("\(DashFormat.count(account.tokensIn &+ account.tokensOut)) tok", width: 58)
                }
            }
            AnalyticsCaption(text: cacheCaption)
        }
        .padding(.top, 2)
    }
}

// MARK: - Client row (#68: parsed labels, zero-suppressed columns)

struct UsageClientRow: View {
    let client: LlmuxDashboardClientUsage
    let now: Date

    var body: some View {
        HStack(spacing: 8) {
            // Raw wire id (may be a metadata.user_id JSON blob) → short human
            // label; non-JSON ids render as-is (#68).
            Text(ClientIDLabel.display(client.client))
                .font(.system(size: 11, weight: .semibold, design: .monospaced))
                .foregroundColor(.white.opacity(0.85))
                .lineLimit(1)
                .truncationMode(.middle)
            Spacer()
            MetricText("\(DashFormat.count(client.requests)) req", width: 54)
            MetricText("\(DashFormat.count(client.tokensIn &+ client.tokensOut)) tok", width: 58)
            MetricText(
                "\(DashFormat.count(client.errors)) err",
                width: 44,
                color: client.errors > 0 ? TerminalColors.red.opacity(0.8) : nil
            )
            // #32 out of scope: cost + last-seen are wire-ready zeros today —
            // absent data is an omitted element, never a `—` column (#68).
            if let cost = DashFormat.clientCost(client.costUsd) {
                MetricText(cost)
            }
            if let seen = DashFormat.clientLastSeen(client.lastSeenMs, now: now) {
                MetricText(seen)
            }
        }
        .padding(8)
        .background(Color.white.opacity(0.04))
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
                        .foregroundColor(.white.opacity(0.6))
                        .frame(width: 28, alignment: .leading)
                    HStack(spacing: 2) {
                        ForEach(cells) { cell in
                            Rectangle()
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
        .padding(8)
        .background(Color.white.opacity(0.04))
    }

    /// One neutral quantitative scale; provider/group identity remains text.
    private func color(for cell: LlmuxDashboardWindowedCell, peak: UInt64) -> Color {
        let intensity = peak > 0 ? Double(cell.tokens) / Double(peak) : 0
        return Color.white.opacity(0.14 + 0.72 * intensity)
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
        .padding(8)
        .background(Color.white.opacity(0.04))
    }

    @ViewBuilder private func completedRow(_ entry: LlmuxDashboardCompleted) -> some View {
        if entry.isNote {
            // Operator note from the daemon ("token refreshed: …") — server
            // telemetry, not request content.
            HStack(spacing: 6) {
                timeText(DashFormat.ago(ms: entry.atMs, now: now))
                Text(entry.text ?? "note")
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundColor(entry.error == true ? TerminalColors.amber : .white.opacity(0.6))
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
                .foregroundColor(.white.opacity(0.6))
                .lineLimit(1)
        }
    }

    private func timeText(_ value: String) -> some View {
        Text(value)
            .font(.system(size: 9, design: .monospaced))
            .foregroundColor(.white.opacity(0.5))
            .frame(width: 30, alignment: .trailing)
    }

    /// A missing status is an omitted element (blank column), never `—` (#68);
    /// green/amber/red map to 2xx–3xx / 4xx / 5xx — genuine states only.
    private func statusText(_ status: Int?) -> Text {
        guard let status else { return Text("") }
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
    let privacyAlias: String
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
        .background(Color.white.opacity(isExpanded ? 0.08 : 0.04))
    }

    private var header: some View {
        HStack(spacing: 8) {
            Image(systemName: isAuthFailed ? "exclamationmark.octagon.fill" : isOverQuota ? "exclamationmark.triangle.fill" : "circle.fill")
                .font(.system(size: isAuthFailed || isOverQuota ? 9 : 6))
                .foregroundColor(isAuthFailed ? TerminalColors.red : isOverQuota ? TerminalColors.amber : .white.opacity(0.72))
                .accessibilityLabel(isAuthFailed ? "Authentication failed" : isOverQuota ? "Quota warning" : "Healthy")
            EmailPixelized(
                isActive: emailAnonymousEnabled && account.name.contains("@"),
                cacheKey: account.name,
                accessibilityLabel: privacyAlias
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
                .foregroundColor(.white.opacity(0.5))
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
        Text(group.capitalized)
            .font(.caption2.weight(.semibold))
            .foregroundColor(.white.opacity(0.6))
            .padding(.horizontal, 5)
            .padding(.vertical, 2)
            .background(
                Rectangle()
                    .fill(Color.white.opacity(0.08))
            )
    }
}

/// Right-aligned monospaced numeric cell. A non-nil `width` pins the cell so
/// the numeric columns line up across rows (one spacing grid); `color`
/// overrides only for genuine warn states (errors > 0).
struct MetricText: View {
    let value: String
    var width: CGFloat?
    var color: Color?

    init(_ value: String, width: CGFloat? = nil, color: Color? = nil) {
        self.value = value
        self.width = width
        self.color = color
    }

    var body: some View {
        Text(value)
            .font(.system(size: 9, weight: .semibold, design: .monospaced))
            .foregroundColor(color ?? .white.opacity(0.5))
            .lineLimit(1)
            .frame(width: width, alignment: .trailing)
    }
}

struct StatText: View {
    let label: String
    let value: String

    var body: some View {
        HStack(spacing: 4) {
            Text(label)
                .font(.system(size: 9, design: .monospaced))
                .foregroundColor(.white.opacity(0.5))
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
            .foregroundColor(.white.opacity(0.5))
            .lineLimit(1)
    }
}
