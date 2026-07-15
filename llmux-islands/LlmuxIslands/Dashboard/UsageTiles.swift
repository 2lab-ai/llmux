import SwiftUI
import AppKit

enum UsageProvider: Hashable {
    case claude
    case codex
    case grok
    case gemini

    var displayName: String {
        switch self {
        case .claude: "Claude"
        case .codex: "Codex"
        case .grok: "Grok"
        case .gemini: "Gemini"
        }
    }
}

enum UsageWindow: CaseIterable {
    case fiveHour
    case twentyFourHour
    case sevenDay
    case fableWeekly

    var label: String {
        switch self {
        case .fiveHour: "5h"
        case .twentyFourHour: "24h"
        case .sevenDay: "7d"
        // Fable weekly is a 7-day window scoped to the Fable model — stack the
        // duration over the scope so the row reads "7d / Fab" (two lines).
        case .fableWeekly: "7d\nFab"
        }
    }
}

private enum UsageAccountIdFormatter {
    static func displayAccountId(provider: UsageProvider, email: String?, claudeIsTeam: Bool?) -> String? {
        guard let emailSlug = emailSlug(email) else { return nil }

        switch provider {
        case .claude:
            if claudeIsTeam == true {
                return "acct_claude_team_\(emailSlug)"
            }
            return "acct_claude_\(emailSlug)"
        case .codex:
            return "acct_codex_\(emailSlug)"
        case .grok:
            return "acct_grok_\(emailSlug)"
        case .gemini:
            return "acct_gemini_\(emailSlug)"
        }
    }

    private static func emailSlug(_ email: String?) -> String? {
        guard let email = email?.trimmingCharacters(in: .whitespacesAndNewlines).nonEmptyOrNil else { return nil }

        let lowered = email.lowercased()
        var output: [UInt8] = []
        output.reserveCapacity(lowered.utf8.count)

        var lastWasUnderscore = false
        for byte in lowered.utf8 {
            let isDigit = byte >= 48 && byte <= 57
            let isLower = byte >= 97 && byte <= 122
            if isDigit || isLower {
                output.append(byte)
                lastWasUnderscore = false
            } else {
                guard !lastWasUnderscore else { continue }
                output.append(95) // "_"
                lastWasUnderscore = true
            }
        }

        let raw = String(decoding: output, as: UTF8.self)
        let trimmed = raw.trimmingCharacters(in: CharacterSet(charactersIn: "_"))
        return trimmed.nonEmptyOrNil
    }
}

struct UsageAccountTile: Identifiable {
    let id: String
    let provider: UsageProvider
    let accountId: String
    let label: String
    let email: String?
    let tier: String?
    let claudeIsTeam: Bool?
    let tokenRefresh: TokenRefreshInfo?
    let info: CLIUsageInfo?
    let errorMessage: String?
    let issue: UsageIssue?
    /// Canonical presentation state. Defaults keep standalone previews and
    /// older fixture construction source-compatible.
    var current: Bool = false
    var paused: Bool = false
    var healthy: Bool = true
    var status: String = "active"
    var inFlight: Int = 0
}

private struct UsageAccountTileRowHeightsPreferenceKey: PreferenceKey {
    static var defaultValue: [Int: CGFloat] = [:]

    static func reduce(value: inout [Int: CGFloat], nextValue: () -> [Int: CGFloat]) {
        for (rowIndex, rowHeight) in nextValue() {
            value[rowIndex] = max(value[rowIndex] ?? 0, rowHeight)
        }
    }
}

struct UsageAccountTileGrid: View {
    let tiles: [UsageAccountTile]
    let columns: [GridItem]
    let now: Date
    var onEditClaudeCodeToken: ((String) -> Void)? = nil
    var onClearClaudeCodeToken: ((String) -> Void)? = nil
    var claudeCodeTokenStatusByAccountId: [String: ClaudeCodeTokenStatus] = [:]
    var onSetClaudeCodeTokenEnabled: ((String, Bool) -> Void)? = nil
    var onRemove: ((String) -> Void)? = nil
    var onSetPaused: ((String, Bool) -> Void)? = nil

    @State private var rowHeights: [Int: CGFloat] = [:]

    private struct IndexedTile: Identifiable {
        let index: Int
        let tile: UsageAccountTile

        var id: String { tile.id }
    }

    var body: some View {
        let indexedTiles = tiles.enumerated().map { IndexedTile(index: $0.offset, tile: $0.element) }
        LazyVGrid(columns: columns, spacing: 10) {
            ForEach(indexedTiles, id: \.id) { indexed in
                let rowIndex = rowIndex(for: indexed.index)
                UsageAccountTileCard(
                    tile: indexed.tile,
                    now: now,
                    forcedHeight: rowHeights[rowIndex],
                    rowIndex: rowIndex,
                    onEditClaudeCodeToken: onEditClaudeCodeToken,
                    onClearClaudeCodeToken: onClearClaudeCodeToken,
                    claudeCodeTokenStatus: indexed.tile.provider == .claude
                        ? claudeCodeTokenStatusByAccountId[indexed.tile.accountId]
                        : nil,
                    onSetClaudeCodeTokenEnabled: onSetClaudeCodeTokenEnabled
                )
                .contextMenu {
                    if let onSetPaused {
                        Button(indexed.tile.paused ? "Resume \(indexed.tile.label)" : "Pause \(indexed.tile.label)") {
                            onSetPaused(indexed.tile.accountId, !indexed.tile.paused)
                        }
                    }
                    if let onRemove {
                        Button("Remove \(indexed.tile.label)", role: .destructive) {
                            onRemove(indexed.tile.accountId)
                        }
                    }
                }
            }
        }
        .onPreferenceChange(UsageAccountTileRowHeightsPreferenceKey.self) { newHeights in
            if rowHeights != newHeights {
                rowHeights = newHeights
            }
        }
    }

    private func rowIndex(for tileIndex: Int) -> Int {
        guard !columns.isEmpty else { return 0 }
        return tileIndex / columns.count
    }
}

private struct UsageAccountTileCard: View {
    let tile: UsageAccountTile
    let now: Date
    let forcedHeight: CGFloat?
    let rowIndex: Int?
    let onEditClaudeCodeToken: ((String) -> Void)?
    let onClearClaudeCodeToken: ((String) -> Void)?
    let claudeCodeTokenStatus: ClaudeCodeTokenStatus?
    let onSetClaudeCodeTokenEnabled: ((String, Bool) -> Void)?

    @State private var isHovered = false

    init(
        tile: UsageAccountTile,
        now: Date,
        forcedHeight: CGFloat? = nil,
        rowIndex: Int? = nil,
        onEditClaudeCodeToken: ((String) -> Void)?,
        onClearClaudeCodeToken: ((String) -> Void)?,
        claudeCodeTokenStatus: ClaudeCodeTokenStatus?,
        onSetClaudeCodeTokenEnabled: ((String, Bool) -> Void)?
    ) {
        self.tile = tile
        self.now = now
        self.forcedHeight = forcedHeight
        self.rowIndex = rowIndex
        self.onEditClaudeCodeToken = onEditClaudeCodeToken
        self.onClearClaudeCodeToken = onClearClaudeCodeToken
        self.claudeCodeTokenStatus = claudeCodeTokenStatus
        self.onSetClaudeCodeTokenEnabled = onSetClaudeCodeTokenEnabled
    }

    var body: some View {
        content
            // Keep the measured content at its natural height even when we wrap it with a fixed row height.
            .fixedSize(horizontal: false, vertical: true)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(heightReporter)
            .frame(height: forcedHeight, alignment: .topLeading)
            .background(
                RoundedRectangle(cornerRadius: 10)
                    .fill(isHovered ? Color.white.opacity(0.09) : Color.white.opacity(0.06))
            )
            // Paused accounts read sepia — desaturate, tint warm, dim — so the
            // parked state is visible at a glance (resume via context menu).
            .saturation(tile.paused ? 0 : 1)
            .colorMultiply(tile.paused ? Color(red: 1.0, green: 0.9, blue: 0.72) : .white)
            .opacity(tile.paused ? 0.8 : 1)
            .onHover { isHovered = $0 }
            .animation(.easeInOut(duration: 0.15), value: isHovered)
    }

    private var content: some View {
        VStack(alignment: .leading, spacing: 8) {
            UsageProviderColumn(
                provider: tile.provider,
                accountId: tile.accountId,
                email: tile.email,
                tier: tile.tier,
                claudeIsTeam: tile.claudeIsTeam,
                tokenRefresh: tile.tokenRefresh,
                info: tile.info,
                now: now,
                onEditClaudeCodeToken: onEditClaudeCodeToken,
                onClearClaudeCodeToken: onClearClaudeCodeToken,
                claudeCodeTokenStatus: claudeCodeTokenStatus,
                onSetClaudeCodeTokenEnabled: onSetClaudeCodeTokenEnabled
            )

            if let issue = tile.issue {
                UsageIssueInlineView(issue: issue)
            } else {
                Text((tile.errorMessage?.trimmingCharacters(in: .whitespacesAndNewlines).nonEmptyOrNil) ?? " ")
                    .font(.system(size: 10))
                    .foregroundColor(TerminalColors.amber.opacity(0.9))
                    .lineLimit(1)
                    .opacity((tile.errorMessage?.trimmingCharacters(in: .whitespacesAndNewlines).nonEmptyOrNil) == nil ? 0 : 1)
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 10)
    }

    @ViewBuilder
    private var heightReporter: some View {
        if let rowIndex {
            GeometryReader { proxy in
                Color.clear.preference(
                    key: UsageAccountTileRowHeightsPreferenceKey.self,
                    value: [rowIndex: proxy.size.height]
                )
            }
        }
    }
}

private struct UsageIssueInlineView: View {
    let issue: UsageIssue

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(issue.message)
                .font(.system(size: 10))
                .foregroundColor(TerminalColors.amber.opacity(0.9))
                .lineLimit(2)
        }
    }
}

private struct UsageProviderColumn: View {
    let provider: UsageProvider
    let accountId: String?
    let email: String?
    let tier: String?
    let claudeIsTeam: Bool?
    let tokenRefresh: TokenRefreshInfo?
    let info: CLIUsageInfo?
    let now: Date
    let onEditClaudeCodeToken: ((String) -> Void)?
    let onClearClaudeCodeToken: ((String) -> Void)?
    let claudeCodeTokenStatus: ClaudeCodeTokenStatus?
    let onSetClaudeCodeTokenEnabled: ((String, Bool) -> Void)?

    @AppStorage(AppSettings.emailAnonymousEnabledKey) private var emailAnonymousEnabled = false
    @AppStorage(AppSettings.showFableWeeklyKey) private var showFableWeekly = true

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            header

            UsageTokenRefreshRow(tokenRefresh: tokenRefresh, now: now)

            usageRows
        }
        .contextMenu {
            if provider == .claude, let accountId = normalizedAccountId {
                if let onEditClaudeCodeToken {
                    Button("Set Claude Code Token…") {
                        onEditClaudeCodeToken(accountId)
                    }
                }

                if let onClearClaudeCodeToken {
                    Button("Clear Claude Code Token") {
                        onClearClaudeCodeToken(accountId)
                    }
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                UsageProviderIcon(provider: provider, size: 16)

                // The header title is the account email whenever one is known —
                // mosaic it when "Email anonymous" is on (todo item 3). It rides
                // the icon row: a separate title line wasted a row per tile.
                EmailPixelized(
                    isActive: emailAnonymousEnabled && normalizedEmail != nil,
                    cacheKey: headerTitle
                ) {
                    Text(headerTitle)
                        .font(.system(size: headerTitleFontSize, weight: .semibold, design: .monospaced))
                        .foregroundColor(headerTitleColor)
                        .lineLimit(1)
                        .truncationMode(.middle)
                        .minimumScaleFactor(0.35)
                        .allowsTightening(true)
                }

                Spacer(minLength: 0)

                if let tier = tierBadgeTier {
                    TierBadge(provider: provider, tier: tier)
                }

                if showsClaudeTeamBadge {
                    Text("TEAM")
                        .font(.system(size: 9, weight: .semibold, design: .monospaced))
                        .foregroundColor(Color.white.opacity(0.7))
                        .lineLimit(1)
                        .fixedSize(horizontal: true, vertical: false)
                        .padding(.horizontal, 8)
                        .padding(.vertical, 4)
                        .background(
                            Capsule(style: .continuous)
                                .fill(Color.white.opacity(0.08))
                        )
                }

                if let badge = statusBadge {
                    Text(badge.label)
                        .font(.system(size: 9, weight: .semibold, design: .monospaced))
                        .foregroundColor(badge.foreground)
                        .lineLimit(1)
                        .fixedSize(horizontal: true, vertical: false)
                        .padding(.horizontal, 8)
                        .padding(.vertical, 4)
                        .background(
                            Capsule(style: .continuous)
                                .fill(badge.background)
                        )
                }
            }
        }
    }

    private var headerTitleFontSize: CGFloat {
        let count = headerTitle.count
        switch count {
        case 0...16: return 14
        case 17...26: return 13
        case 27...38: return 12
        default: return 11
        }
    }

    private var tierBadgeTier: String? {
        guard let tier = resolvedTier else { return nil }

        // Only show Claude tier when we can confidently classify it.
        if provider == .claude, normalizedClaudeTierLabel(from: tier) == nil {
            return nil
        }

        return tier
    }

    private func normalizedClaudeTierLabel(from tier: String) -> String? {
        let raw = tier.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !raw.isEmpty else { return nil }

        let lowered = raw.lowercased()
        let tokens = lowered.split { !($0.isLetter || $0.isNumber) }
        let hasToken: (String) -> Bool = { token in tokens.contains { $0 == token } }
        let normalized = lowered
            .replacingOccurrences(of: " ", with: "")
            .replacingOccurrences(of: "-", with: "")
            .replacingOccurrences(of: "_", with: "")

        if normalized.contains("max20") || (hasToken("max") && (hasToken("20x") || hasToken("20"))) { return "Max20" }
        if normalized.contains("max5") || (hasToken("max") && (hasToken("5x") || hasToken("5"))) { return "Max5" }
        if hasToken("pro") { return "Pro" }
        if hasToken("max") || normalized.contains("max") { return "Max" }

        return nil
    }

    private var showsClaudeTeamBadge: Bool {
        provider == .claude && claudeIsTeam == true
    }

    private var normalizedAccountId: String? {
        accountId?.trimmingCharacters(in: .whitespacesAndNewlines).nonEmptyOrNil
    }

    private var normalizedEmail: String? {
        email?.trimmingCharacters(in: .whitespacesAndNewlines).nonEmptyOrNil
    }

    private var headerTitle: String {
        if let normalizedEmail { return normalizedEmail }
        if let info, !info.available { return "Not installed" }
        if let normalizedAccountId { return normalizedAccountId }
        return "--"
    }

    private var headerTitleColor: Color {
        if normalizedEmail != nil { return Color.white.opacity(0.9) }
        if let info, !info.available { return TerminalColors.dim }
        if normalizedAccountId != nil { return Color.white.opacity(0.22) }
        return Color.white.opacity(0.2)
    }

    private var statusBadge: (label: String, background: Color, foreground: Color)? {
        if let info, !info.available {
            return (label: "MISS", background: Color.white.opacity(0.08), foreground: Color.white.opacity(0.45))
        }

        if isTokenExpired {
            return (label: "EXP", background: TerminalColors.amber.opacity(0.9), foreground: Color.black.opacity(0.85))
        }

        if info?.error == true {
            return (label: "ERR", background: TerminalColors.red.opacity(0.9), foreground: Color.white.opacity(0.9))
        }
        return nil
    }

    private var isTokenExpired: Bool {
        guard let tokenRefresh else { return false }
        return tokenRefresh.expiresAt <= now
    }

    private var resolvedTier: String? {
        switch provider {
        case .claude:
            return tier
        case .codex:
            return normalizeCodexTier(info?.plan)
        case .grok:
            // Grok exposes no plan/tier over the llmux status API.
            return nil
        case .gemini:
            return inferGeminiTier(model: info?.model, plan: info?.plan)
        }
    }

    @ViewBuilder
    private var usageRows: some View {
        switch provider {
        case .gemini:
            GeminiUsageSummaryRow(info: info, now: now)
        case .claude, .codex, .grok:
            ForEach(providerWindows, id: \.label) { window in
                UsageWindowRow(
                    window: window,
                    percentUsed: percentUsed(for: window),
                    resetAt: resetAt(for: window),
                    now: now,
                    emphasizeCritical: emphasizeCritical(for: window)
                )
            }
        }
    }

    private var providerWindows: [UsageWindow] {
        switch provider {
        case .gemini:
            return []
        case .claude, .codex, .grok:
            // Grok reports no quota windows (docs/grok/spec.md §R3) — the
            // rows render their placeholder until a parked reset appears.
            var windows: [UsageWindow] = [.fiveHour, .sevenDay]
            // The Fable weekly row is opt-out (default on) and only appears when
            // the daemon actually reports a Fable weekly window for this account.
            if showFableWeekly, info?.fableWeeklyPercent != nil {
                windows.append(.fableWeekly)
            }
            return windows
        }
    }

    private func percentUsed(for window: UsageWindow) -> Double? {
        guard let info, info.available, !info.error else { return nil }
        switch window {
        case .fiveHour, .twentyFourHour: return info.fiveHourPercent
        case .sevenDay: return info.sevenDayPercent
        case .fableWeekly: return info.fableWeeklyPercent
        }
    }

    private func resetAt(for window: UsageWindow) -> Date? {
        guard let info, info.available, !info.error else { return nil }
        switch window {
        case .fiveHour, .twentyFourHour: return info.fiveHourReset
        case .sevenDay: return info.sevenDayReset
        case .fableWeekly: return info.fableWeeklyReset
        }
    }

    /// The Fable weekly row is emphasized (red) only when the daemon's
    /// reset-aware `constraining` bool is true. This keys off `constraining`
    /// (from `ScopedQuotaWindow::is_constraining`), NOT the raw `severity`
    /// string: `severity` is not reset-aware, so a just-reset window can still
    /// carry a stale `critical` while its utilization is 0 — keying red off it
    /// would flash `F 0%!` right after a weekly reset. `is_active` is likewise
    /// NOT a trigger: it marks the representative/governing limit, NOT an
    /// exhausted one (a 76%/warning row is `is_active: true` with ~24%
    /// headroom). `constraining` folds both concerns in on the daemon side.
    private func emphasizeCritical(for window: UsageWindow) -> Bool {
        guard window == .fableWeekly, let info else { return false }
        return info.fableWeeklyConstraining == true
    }

    private func normalizeCodexTier(_ plan: String?) -> String? {
        guard let plan = plan?.trimmingCharacters(in: .whitespacesAndNewlines).nonEmptyOrNil else { return nil }

        let lowered = plan.lowercased()
        let tokens = lowered.split { !($0.isLetter || $0.isNumber) }
        let hasToken: (String) -> Bool = { token in tokens.contains { $0 == token } }

        if hasToken("plus") || lowered.contains("plus") { return "Plus" }
        if hasToken("pro") || lowered.contains("pro") { return "Pro" }
        return plan
    }

    private func inferGeminiTier(model: String?, plan: String?) -> String? {
        let candidates = [plan, model]
            .compactMap { $0?.trimmingCharacters(in: .whitespacesAndNewlines).nonEmptyOrNil }
        guard !candidates.isEmpty else { return nil }

        let lowered = candidates.joined(separator: " ").lowercased()
        if lowered.contains("pro") { return "Pro" }
        if lowered.contains("flash") { return "Flash" }
        if lowered.contains("ultra") { return "Ultra" }
        if lowered.contains("nano") { return "Nano" }
        return nil
    }
}

private struct TierBadge: View {
    let provider: UsageProvider
    let tier: String

    var body: some View {
        Text(label)
            .font(.system(size: 9, weight: .semibold, design: .monospaced))
            .foregroundColor(style.foreground)
            .lineLimit(1)
            .minimumScaleFactor(0.75)
            .fixedSize(horizontal: true, vertical: false)
            .padding(.horizontal, 8)
            .padding(.vertical, 4)
            .background(
                Capsule(style: .continuous)
                    .fill(style.background)
            )
    }

    private var label: String {
        let lowered = tier.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        if lowered.contains("max") && lowered.contains("20") { return "Max20" }
        if lowered.contains("max") && lowered.contains("5") { return "Max5" }
        if lowered.contains("max") { return "Max" }
        if lowered.contains("plus") { return "Plus" }
        if lowered.contains("pro") { return "Pro" }
        if lowered.contains("flash") { return "Flash" }
        if lowered.contains("ultra") { return "Ultra" }
        if lowered.contains("nano") { return "Nano" }
        return tier
    }

    private var style: (background: Color, foreground: Color) {
        let key = label.lowercased()

        switch provider {
        case .claude:
            if key == "pro" { return (Color.white.opacity(0.9), Color.black.opacity(0.85)) }
            if key == "max5" { return (TerminalColors.amber, Color.black.opacity(0.85)) }
            if key == "max20" { return (TerminalColors.red, Color.white.opacity(0.9)) }
            if key == "max" { return (TerminalColors.red.opacity(0.85), Color.white.opacity(0.9)) }
        case .codex:
            if key == "plus" { return (Color.white.opacity(0.9), Color.black.opacity(0.85)) }
            if key == "pro" { return (TerminalColors.red, Color.white.opacity(0.9)) }
        case .grok:
            break
        case .gemini:
            return (TerminalColors.blue.opacity(0.85), Color.white.opacity(0.9))
        }

        return (Color.white.opacity(0.08), Color.white.opacity(0.55))
    }
}

private struct UsageTokenRefreshRow: View {
    let tokenRefresh: TokenRefreshInfo?
    let now: Date

    var body: some View {
        HStack(alignment: .center, spacing: 12) {
            Image(systemName: "key.fill")
                .font(.system(size: 13, weight: .semibold))
                .foregroundColor(iconColor)
                .frame(width: 26, alignment: .leading)

            MiniSegmentBar(
                fraction: remainingFraction,
                fillColor: barFillColor,
                emptyColor: Color.white.opacity(0.08)
            )
            .frame(height: 10)
            .frame(maxWidth: .infinity)

            timeRemainingText
                .lineLimit(1)
                .minimumScaleFactor(0.7)
                .frame(minWidth: 84, alignment: .trailing)
        }
        .opacity(tokenRefresh == nil ? 0.65 : 1)
    }

    private var remainingFraction: Double {
        guard let tokenRefresh else { return 0 }
        let remaining = max(0, tokenRefresh.expiresAt.timeIntervalSince(now))
        let total = max(1, tokenRefresh.lifetimeSeconds)
        return max(0, min(1, remaining / total))
    }

    private var barFillColor: Color {
        tokenRefresh == nil
            ? Color.white.opacity(0.12)
            : TerminalColors.magenta.opacity(0.85)
    }

    private var iconColor: Color {
        tokenRefresh == nil
            ? Color.white.opacity(0.25)
            : TerminalColors.magenta.opacity(0.85)
    }

    private var timeRemainingText: Text {
        let baseColor = Color.white.opacity(0.32)
        let font = Font.system(size: 14, weight: .semibold, design: .monospaced)
        guard let tokenRefresh else { return Text("--").font(font).foregroundColor(baseColor) }
        if tokenRefresh.expiresAt <= now {
            return Text("Expired!")
                .font(font)
                .foregroundColor(TerminalColors.amber.opacity(0.9))
        }
        let seconds = max(0, Int(tokenRefresh.expiresAt.timeIntervalSince(now)))
        return UsageDurationText.make(seconds: seconds, digitColor: baseColor, scale: 1.3)
    }
}

private struct GeminiUsageSummaryRow: View {
    let info: CLIUsageInfo?
    let now: Date

    var body: some View {
        HStack(spacing: 8) {
            Text(modelName)
                .foregroundColor(.white.opacity(0.6))
                .lineLimit(1)
                .truncationMode(.middle)

            Spacer(minLength: 8)

            Text(bucketCountString)
                .foregroundColor(.white.opacity(0.35))
                .frame(width: 14, alignment: .trailing)

            Text(remainingPercentString)
                .foregroundColor(remainingPercentColor)
                .frame(width: 54, alignment: .trailing)

            resetsInText
                .lineLimit(1)
                .minimumScaleFactor(0.7)
        }
        .font(.system(size: 10, weight: .semibold, design: .monospaced))
    }

    private var modelName: String {
        info?.model?.trimmingCharacters(in: .whitespacesAndNewlines).nonEmptyOrNil ?? "gemini"
    }

    private var bucketCountString: String {
        guard let buckets = info?.buckets else { return "--" }
        return "\(buckets.count)"
    }

    private var remainingPercentString: String {
        guard let used = info?.fiveHourPercent else { return "--" }
        let remaining = max(0, min(100, 100 - used))
        return String(format: "%.1f%%", remaining)
    }

    private var remainingPercentColor: Color {
        guard let used = info?.fiveHourPercent else { return TerminalColors.dim }
        let remaining = max(0, min(100, 100 - used))
        if remaining < 10 { return TerminalColors.red }
        if remaining < 25 { return TerminalColors.amber }
        return TerminalColors.green
    }

    private var resetsInText: Text {
        let baseColor = Color.white.opacity(0.28)
        guard let resetAt = info?.fiveHourReset else {
            return Text("(Resets in --)")
                .foregroundColor(baseColor)
        }

        let seconds = max(0, Int(resetAt.timeIntervalSince(now)))
        return Text("(").foregroundColor(baseColor)
            + Text("Resets in ").foregroundColor(baseColor)
            + UsageDurationText.make(seconds: seconds, digitColor: baseColor)
            + Text(")").foregroundColor(baseColor)
    }
}

private struct UsageWindowRow: View {
    let window: UsageWindow
    let percentUsed: Double?
    let resetAt: Date?
    let now: Date
    /// When true (Fable weekly at `severity == critical`), the usage bar +
    /// percent are forced red regardless of the usage-fraction hue ramp.
    /// `is_active` alone does NOT set this — it marks the representative limit,
    /// not an exhausted one, so a 76%/warning/is_active row keeps its normal hue.
    var emphasizeCritical: Bool = false

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            Text(window.label)
                .font(.system(size: 14, weight: .semibold, design: .monospaced))
                .foregroundColor(labelColor)
                .lineLimit(2)
                .minimumScaleFactor(0.8)
                .frame(width: 26, alignment: .leading)
                .padding(.top, 2)

            // Usage-remaining column (bar over percent), stretches to fill width.
            VStack(spacing: 6) {
                MiniSegmentBar(
                    fraction: usageRemainingFraction,
                    fillColor: usageFillColor,
                    emptyColor: Color.white.opacity(0.08)
                )
                .frame(height: 10)

                Text(remainingPercentString)
                    .font(.system(size: 14, weight: .semibold, design: .monospaced))
                    .foregroundColor(usageTextColor)
                    .lineLimit(1)
                    .minimumScaleFactor(0.7)
            }
            .frame(maxWidth: .infinity)

            // Reset-countdown column (bar over remaining time), stretches to fill width.
            VStack(spacing: 6) {
                MiniSegmentBar(
                    fraction: resetRemainingFraction,
                    fillColor: TerminalColors.blue.opacity(0.85),
                    emptyColor: Color.white.opacity(0.08)
                )
                .frame(height: 10)

                timeRemainingText
                    .lineLimit(1)
                    .minimumScaleFactor(0.7)
            }
            .frame(maxWidth: .infinity)
        }
    }

    private var usageRemainingFraction: Double {
        guard let percentUsed else { return 0 }
        let used = max(0, min(100, percentUsed))
        return max(0, min(1, (100 - used) / 100))
    }

    private var remainingPercentString: String {
        guard let percentUsed else { return "--" }
        let used = max(0, min(100, percentUsed))
        let remaining = max(0, min(100, 100 - used))
        return "\(Int(remaining.rounded()))%"
    }

    private var labelColor: Color {
        emphasizeCritical ? TerminalColors.red : .white.opacity(0.45)
    }

    private var usageFillColor: Color {
        if emphasizeCritical { return TerminalColors.red }
        let fraction = max(0, min(1, usageRemainingFraction))
        let hue = 0.33 * fraction
        return Color(hue: hue, saturation: 0.85, brightness: 0.95)
    }

    private var usageTextColor: Color {
        guard percentUsed != nil else { return TerminalColors.dim }
        if emphasizeCritical { return TerminalColors.red }
        return usageFillColor.opacity(0.9)
    }

    private var resetRemainingFraction: Double {
        guard let resetAt, let total = windowDurationSeconds else { return 0 }
        let remaining = max(0, resetAt.timeIntervalSince(now))
        return max(0, min(1, remaining / total))
    }

    private var timeRemainingText: Text {
        let baseColor = Color.white.opacity(0.32)
        guard let resetAt else {
            return Text("--").font(.system(size: 14, weight: .semibold, design: .monospaced)).foregroundColor(baseColor)
        }
        let seconds = max(0, Int(resetAt.timeIntervalSince(now)))
        return UsageDurationText.make(seconds: seconds, digitColor: baseColor, scale: 1.3)
    }

    private var windowDurationSeconds: TimeInterval? {
        switch window {
        case .fiveHour:
            return 5 * 60 * 60
        case .twentyFourHour:
            return 24 * 60 * 60
        case .sevenDay, .fableWeekly:
            return 7 * 24 * 60 * 60
        }
    }
}

private extension String {
    var nonEmptyOrNil: String? {
        let trimmed = trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? nil : trimmed
    }
}

// MARK: - Compact account rows (issue #68 — Statistics overview)

/// Per-account rows at TUI-accounts-table density: 1–2 lines per account.
/// This is the shared common-path account summary for Usage and Statistics.
struct UsageAccountCompactList: View {
    let tiles: [UsageAccountTile]
    var onRemove: ((String) -> Void)? = nil

    var body: some View {
        VStack(spacing: 4) {
            ForEach(Array(tiles.enumerated()), id: \.element.id) { index, tile in
                UsageAccountCompactRow(
                    tile: tile,
                    privacyAlias: IslandPresentationPolicy.privateAccountLabel(
                        providerName: tile.provider.displayName,
                        ordinal: index + 1
                    )
                )
                    .contextMenu {
                        if let onRemove {
                            Button("Remove \(tile.label)", role: .destructive) {
                                onRemove(tile.accountId)
                            }
                        }
                    }
            }
        }
    }
}

/// One account: status dot + provider mark + masked name, then a thin
/// CONTINUOUS capsule bar per REPORTED window (5h / 7d / Fable-only-when-the-
/// daemon-reports-it) with a small used-% label. Cold/absent windows are
/// omitted entirely — no dashes, no empty gauges. One color semantic: neutral
/// fill for normal utilization, warning red only at ≥ 90% (Fable additionally
/// requires the daemon's reset-aware `constraining` — same rule as
/// `DashboardHealth`).
struct UsageAccountCompactRow: View {
    let tile: UsageAccountTile
    let privacyAlias: String

    @AppStorage(AppSettings.emailAnonymousEnabledKey) private var emailAnonymousEnabled = false
    @AppStorage(AppSettings.showFableWeeklyKey) private var showFableWeekly = true

    private struct Gauge: Identifiable {
        let id: String
        let usedFraction: Double
        let warning: Bool
    }

    private var displayName: String { tile.email ?? tile.label }

    private var isBroken: Bool { tile.info?.error == true || tile.info?.available == false }

    private var allGauges: [Gauge] {
        guard let info = tile.info, info.available, !info.error else { return [] }
        var result: [Gauge] = []
        if let five = info.fiveHourPercent {
            result.append(gauge("5h", usedPercent: five))
        }
        if let seven = info.sevenDayPercent {
            result.append(gauge("7d", usedPercent: seven))
        }
        if showFableWeekly, let fable = info.fableWeeklyPercent {
            // Fable is scoped: red only while the daemon marks it constraining.
            result.append(gauge("Fab", usedPercent: fable, warningOverride: info.fableWeeklyConstraining == true))
        }
        return result
    }

    /// The common path shows one decision-making quota only. A warning or
    /// constraining window wins; otherwise the familiar 5-hour window wins.
    /// Exact secondary windows remain available in the local Advanced panel.
    private var gauges: [Gauge] {
        if let warning = allGauges.first(where: \.warning) {
            return [warning]
        }
        if let fiveHour = allGauges.first(where: { $0.id == "5h" }) {
            return [fiveHour]
        }
        return Array(allGauges.prefix(1))
    }

    private func gauge(_ id: String, usedPercent: Double, warningOverride: Bool? = nil) -> Gauge {
        let fraction = max(0, min(1, usedPercent / 100))
        return Gauge(
            id: id,
            usedFraction: fraction,
            warning: warningOverride ?? (fraction >= DashboardHealth.quotaThreshold)
        )
    }

    private var dotColor: Color {
        if !tile.healthy || isBroken { return TerminalColors.red }
        if gauges.contains(where: \.warning) { return TerminalColors.amber }
        return Color.white.opacity(0.72)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 8) {
                Image(systemName: stateSymbol)
                    .font(.system(size: stateSymbol == "circle.fill" ? 6 : 9, weight: .semibold))
                    .foregroundColor(dotColor)
                    .accessibilityLabel(stateAccessibilityLabel)
                UsageProviderIcon(provider: tile.provider, size: 11)
                EmailPixelized(
                    isActive: emailAnonymousEnabled && displayName.contains("@"),
                    cacheKey: displayName,
                    accessibilityLabel: privacyAlias
                ) {
                    Text(displayName)
                        .font(.caption.weight(.semibold))
                        .foregroundColor(.white.opacity(0.85))
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
                Spacer(minLength: 8)
                if tile.current {
                    stateLabel("Current", inverted: true)
                }
                if tile.paused {
                    stateLabel("Paused", symbol: "pause.fill")
                } else if !tile.healthy || isBroken {
                    stateLabel("Needs attention", symbol: "exclamationmark.triangle.fill", warning: true)
                }
                if tile.inFlight > 0 {
                    stateLabel("Active \(tile.inFlight)", symbol: "waveform")
                }
            }
            if !gauges.isEmpty {
                HStack(spacing: 10) {
                    Spacer(minLength: 25)
                    ForEach(gauges) { gauge in
                        gaugeView(gauge)
                    }
                    Spacer(minLength: 0)
                }
            }
            // Second line only when there is something to say — never a
            // blank placeholder row.
            if let message = tile.errorMessage?.trimmingCharacters(in: .whitespacesAndNewlines),
               !message.isEmpty {
                Text(message)
                    .font(.system(size: 9))
                    .foregroundColor(TerminalColors.amber.opacity(0.9))
                    .lineLimit(1)
                    .padding(.leading, 14)
            } else if let issue = tile.issue {
                Text(issue.message)
                    .font(.system(size: 9))
                    .foregroundColor(TerminalColors.amber.opacity(0.9))
                    .lineLimit(1)
                    .padding(.leading, 14)
            }
        }
        .padding(.horizontal, 8)
        .frame(minHeight: 44)
        .background(Color.white.opacity(0.04))
    }

    private var stateSymbol: String {
        if !tile.healthy || isBroken { return "exclamationmark.octagon.fill" }
        if tile.paused { return "pause.circle.fill" }
        return "circle.fill"
    }

    private var stateAccessibilityLabel: String {
        if !tile.healthy || isBroken { return "Needs attention" }
        if tile.paused { return "Paused" }
        return "Healthy"
    }

    private func stateLabel(
        _ text: String,
        symbol: String? = nil,
        inverted: Bool = false,
        warning: Bool = false
    ) -> some View {
        HStack(spacing: 3) {
            if let symbol {
                Image(systemName: symbol)
                    .font(.system(size: 8, weight: .semibold))
            }
            Text(text)
                .font(.caption2.weight(.semibold))
        }
        .foregroundColor(inverted ? .black : warning ? TerminalColors.amber : .white.opacity(0.6))
        .padding(.horizontal, 5)
        .padding(.vertical, 2)
        .background(inverted ? Color.white : Color.white.opacity(0.06))
    }

    private func gaugeView(_ gauge: Gauge) -> some View {
        HStack(spacing: 4) {
            if gauge.warning {
                Image(systemName: "exclamationmark.triangle.fill")
                    .font(.system(size: 8, weight: .semibold))
                    .foregroundColor(TerminalColors.amber)
                    .accessibilityLabel("Quota warning")
            }
            Text(gauge.id)
                .font(.system(size: 8, weight: .semibold, design: .monospaced))
                .foregroundColor(.white.opacity(0.6))
            Rectangle()
                .fill(Color.white.opacity(0.1))
                .frame(width: 44, height: 3)
                .overlay(alignment: .leading) {
                    Rectangle()
                        .fill(gauge.warning ? TerminalColors.red : Color.white.opacity(0.55))
                        .frame(width: 44 * gauge.usedFraction)
                }
            Text("\(Int((gauge.usedFraction * 100).rounded()))%")
                .font(.system(size: 9, weight: .semibold, design: .monospaced))
                .foregroundColor(gauge.warning ? TerminalColors.red : .white.opacity(0.6))
                .frame(width: 28, alignment: .trailing)
        }
    }
}

private struct MiniSegmentBar: View {
    let fraction: Double
    let fillColor: Color
    let emptyColor: Color

    var body: some View {
        GeometryReader { geo in
            let segmentCount = 10
            let spacing: CGFloat = 1
            let totalSpacing = spacing * CGFloat(segmentCount - 1)
            let segmentWidth = max(1, (geo.size.width - totalSpacing) / CGFloat(segmentCount))
            let filledSegments = max(0, min(segmentCount, Int((fraction * Double(segmentCount)).rounded(.toNearestOrAwayFromZero))))

            HStack(spacing: spacing) {
                ForEach(0..<segmentCount, id: \.self) { index in
                    Rectangle()
                        .fill(index < filledSegments ? fillColor : emptyColor)
                        .frame(width: segmentWidth)
                }
            }
        }
    }
}

private struct ClaudeCodeTokenSheet: View {
    let accountId: String
    let displayAccountId: String
    let email: String?
    @Binding var token: String
    let onCancel: () -> Void
    let onClear: () -> Void
    let onSave: () -> Void

    @AppStorage(AppSettings.emailAnonymousEnabledKey) private var emailAnonymousEnabled = false

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack(spacing: 10) {
                UsageProviderIcon(provider: .claude, size: 16)
                Text("Claude Code Token")
                    .font(.system(size: 16, weight: .semibold))
                Spacer()
            }

            VStack(alignment: .leading, spacing: 6) {
                EmailPixelized(
                    isActive: emailAnonymousEnabled && hasEmail,
                    cacheKey: emailLine
                ) {
                    Text(emailLine)
                        .font(.system(size: 12, weight: .semibold, design: .monospaced))
                        .foregroundColor(.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }

                Text(displayAccountId)
                    .font(.system(size: 11, weight: .semibold, design: .monospaced))
                    .foregroundColor(.secondary.opacity(0.85))
                    .lineLimit(1)
                    .truncationMode(.middle)
            }

            Text("Paste `CLAUDE_CODE_OAUTH_TOKEN` from `claude setup-token`. Stored locally and applied on profile switch. Not used for usage fetching.")
                .font(.system(size: 11))
                .foregroundColor(.secondary)

            SecureField("CLAUDE_CODE_OAUTH_TOKEN", text: $token)
                .textFieldStyle(.roundedBorder)
                .font(.system(size: 12, weight: .medium, design: .monospaced))

            HStack(spacing: 10) {
                Button("Cancel") { onCancel() }
                Spacer()
                Button("Clear", role: .destructive) { onClear() }
                Button("Save") { onSave() }
                    .disabled(token.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    .keyboardShortcut(.defaultAction)
            }
        }
        .padding(16)
        .frame(width: 520)
    }

    private var emailLine: String {
        email?.trimmingCharacters(in: .whitespacesAndNewlines).nonEmptyOrNil ?? "--"
    }

    /// Only mosaic a real email — the "--" placeholder stays readable.
    private var hasEmail: Bool {
        email?.trimmingCharacters(in: .whitespacesAndNewlines).nonEmptyOrNil != nil
    }
}

private struct SaveProfileSheet: View {
    let isSaving: Bool
    @Binding var name: String
    let onCancel: () -> Void
    let onSave: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("Save Profile")
                .font(.system(size: 16, weight: .semibold))

            VStack(alignment: .leading, spacing: 8) {
                Text("Profile Name")
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundColor(.secondary)

                TextField("e.g. Work", text: $name)
                    .textFieldStyle(.roundedBorder)
            }

            Text("This snapshots your current Claude/Codex/Gemini CLI credentials into `~/.agent-island/accounts/` and links them to the profile.")
                .font(.system(size: 11))
                .foregroundColor(.secondary)

            HStack {
                Button("Cancel") { onCancel() }
                Spacer()
                Button(isSaving ? "Saving…" : "Save") { onSave() }
                    .disabled(isSaving || name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    .keyboardShortcut(.defaultAction)
            }
        }
        .padding(16)
        .frame(width: 380)
    }
}
