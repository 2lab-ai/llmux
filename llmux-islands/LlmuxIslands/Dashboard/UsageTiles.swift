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
    case sevenDay

    var label: String {
        switch self {
        case .sevenDay: "7d"
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
    /// Canonical shared-core state is retained for native behavior and future
    /// projections even though the preserved pre-renewal UI does not add new
    /// badges for these fields.
    var current: Bool = false
    /// Operator pause: rendered sepia + context-menu Resume. Defaulted so
    /// non-llmux construction sites stay source-compatible.
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
    @AppStorage(AppSettings.showTokenExpiryKey) private var showTokenExpiry = false

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            header

            // Key-icon row: off by default, ☰ → "Show token expiry".
            if showTokenExpiry {
                UsageTokenRefreshRow(tokenRefresh: tokenRefresh, now: now)
            }

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
        case .claude, .codex, .grok, .gemini:
            // llmux reports gemini quota as the shared 7d window (like codex),
            // so it renders through the same rows.
            ForEach(providerWindows, id: \.label) { window in
                UsageWindowRow(
                    window: window,
                    percentUsed: percentUsed(for: window),
                    resetAt: resetAt(for: window),
                    now: now,
                    fable: fableReading
                )
            }
        }
    }

    private var providerWindows: [UsageWindow] {
        switch provider {
        case .claude, .codex, .grok, .gemini:
            // Only the 7d row: no provider has a 5h window any more (owner
            // 2026-11-04 and 2026-11-10). Grok reports no quota windows (docs/grok/spec.md §R3) — the row
            // renders its placeholder until a parked reset appears.
            // Fable weekly is not a row of its own any more: it rides the 7d
            // row's bar and text (`fableReading`, SevenDayReadout).
            return [.sevenDay]
        }
    }

    /// Fable weekly folded into the 7d row — only when the toggle is on and
    /// the daemon reports the window for this account.
    private var fableReading: UsageWindowRow.FableReading? {
        guard showFableWeekly, let info, info.available, !info.error,
              let used = info.fableWeeklyPercent
        else { return nil }
        return UsageWindowRow.FableReading(
            percentUsed: used,
            resetAt: info.fableWeeklyReset,
            constraining: info.fableWeeklyConstraining == true
        )
    }

    private func percentUsed(for window: UsageWindow) -> Double? {
        guard let info, info.available, !info.error else { return nil }
        switch window {
        case .sevenDay: return info.sevenDayPercent
        }
    }

    private func resetAt(for window: UsageWindow) -> Date? {
        guard let info, info.available, !info.error else { return nil }
        switch window {
        case .sevenDay: return info.sevenDayReset
        }
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

private struct UsageWindowRow: View {
    /// Fable weekly, drawn inside this row (SevenDayReadout).
    struct FableReading: Equatable {
        let percentUsed: Double
        let resetAt: Date?
        /// The daemon's reset-aware `constraining` (from
        /// `ScopedQuotaWindow::is_constraining`) turns the Fable bracket and
        /// its bracketed number red. Not the raw `severity` (a just-reset window can
        /// keep a stale `critical` at 0%), not `is_active` (it marks the
        /// governing limit, not an exhausted one).
        let constraining: Bool
    }

    let window: UsageWindow
    let percentUsed: Double?
    let resetAt: Date?
    let now: Date
    var fable: FableReading? = nil

    /// Lavender: outside the 7d bar's red→green ramp and the blue reset bar,
    /// so the bracket under the bar and the bracketed number read as one
    /// series.
    static let fableColor = Color(red: 0.78, green: 0.64, blue: 1.0)

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            Text(window.label)
                .font(.system(size: 14, weight: .semibold, design: .monospaced))
                .foregroundColor(.white.opacity(0.45))
                .lineLimit(1)
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
                // Fable sits under the bar, never on it: a bracket from the
                // start of the track to Fable's remaining. Drawn in the gap
                // above the text, so the row keeps its height and the two
                // columns stay aligned.
                .overlay(alignment: .topLeading) {
                    if let fableRemainingFraction {
                        FableBracket(remaining: fableRemainingFraction, color: fableTint)
                            .frame(height: FableBracket.height)
                            .offset(y: 10 + FableBracket.gap)
                    }
                }

                remainingText
                    .lineLimit(1)
                    .minimumScaleFactor(0.6)
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
        .help(helpText)
    }

    private var usageRemainingFraction: Double {
        SevenDayReadout.remaining(usedPercent: percentUsed) ?? 0
    }

    private var fableRemainingFraction: Double? {
        fable.flatMap { SevenDayReadout.remaining(usedPercent: $0.percentUsed) }
    }

    private var fableTint: Color {
        fable?.constraining == true ? TerminalColors.red : Self.fableColor
    }

    /// `60%` in the 7d colour, then ` (40%)` smaller in the Fable colour.
    private var remainingText: Text {
        let seven = Text(SevenDayReadout.percentText(SevenDayReadout.remaining(usedPercent: percentUsed)))
            .font(.system(size: 14, weight: .semibold, design: .monospaced))
            .foregroundColor(usageTextColor)
        guard let fableRemainingFraction else { return seven }
        return seven + Text(SevenDayReadout.fableSuffix(fableRemainingFraction))
            .font(.system(size: 11, weight: .semibold, design: .monospaced))
            .foregroundColor(fableTint)
    }

    private var usageFillColor: Color {
        let fraction = max(0, min(1, usageRemainingFraction))
        let hue = 0.33 * fraction
        return Color(hue: hue, saturation: 0.85, brightness: 0.95)
    }

    private var usageTextColor: Color {
        guard percentUsed != nil else { return TerminalColors.dim }
        return usageFillColor.opacity(0.9)
    }

    /// Hover: both values with their own resets (Fable can reset on a
    /// different day than the 7d window shown in the right column).
    private var helpText: String {
        var lines = ["7d: \(SevenDayReadout.percentText(SevenDayReadout.remaining(usedPercent: percentUsed))) left"
            + (resetAt.map { " · resets in \(GlanceResolver.duration(Int($0.timeIntervalSince(now))))" } ?? "")]
        if let fable {
            lines.append("Fable 7d (bracket under the bar): "
                + "\(SevenDayReadout.percentText(fableRemainingFraction)) left"
                + (fable.resetAt.map { " · resets in \(GlanceResolver.duration(Int($0.timeIntervalSince(now))))" } ?? "")
                + (fable.constraining ? " · limiting" : ""))
        }
        return lines.joined(separator: "\n")
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
        case .sevenDay:
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
/// Used ONLY by the Statistics panel's Overview section (issue #68 v2) as the
/// account summary between the totals cards and the top models; the default
/// Usage panel keeps the v0.2.14 `UsageAccountTileGrid`.
struct UsageAccountCompactList: View {
    let tiles: [UsageAccountTile]
    var onRemove: ((String) -> Void)? = nil

    var body: some View {
        VStack(spacing: 4) {
            ForEach(tiles) { tile in
                UsageAccountCompactRow(tile: tile)
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
/// CONTINUOUS capsule bar per REPORTED window (7d / Fable-only-when-the-
/// daemon-reports-it — `CompactUsageGauges`) with a small used-%
/// label. Cold/absent windows are omitted entirely — no dashes, no empty
/// gauges. One color semantic: neutral fill for normal utilization, warning
/// red only at ≥ 90% (Fable additionally requires the daemon's reset-aware
/// `constraining` — same rule as `DashboardHealth`).
struct UsageAccountCompactRow: View {
    let tile: UsageAccountTile

    @AppStorage(AppSettings.emailAnonymousEnabledKey) private var emailAnonymousEnabled = false
    @AppStorage(AppSettings.showFableWeeklyKey) private var showFableWeekly = true

    private typealias Gauge = CompactUsageGauges.Gauge

    private var displayName: String { tile.email ?? tile.label }

    private var isBroken: Bool { tile.info?.error == true || tile.info?.available == false }

    /// The reported usage, when the account answered.
    private var usage: CLIUsageInfo? {
        guard let info = tile.info, info.available, !info.error else { return nil }
        return info
    }

    private var gauges: [Gauge] {
        usage.map { CompactUsageGauges.gauges($0, showFable: showFableWeekly) } ?? []
    }

    private var dotColor: Color {
        if isBroken { return TerminalColors.red }
        if let usage, CompactUsageGauges.warning(usage, showFable: showFableWeekly) {
            return TerminalColors.amber
        }
        return TerminalColors.green
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 8) {
                Circle()
                    .fill(dotColor)
                    .frame(width: 6, height: 6)
                UsageProviderIcon(provider: tile.provider, size: 11)
                EmailPixelized(
                    isActive: emailAnonymousEnabled && displayName.contains("@"),
                    cacheKey: displayName
                ) {
                    Text(displayName)
                        .font(.system(size: 11, weight: .semibold, design: .monospaced))
                        .foregroundColor(.white.opacity(0.85))
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
                Spacer(minLength: 8)
                ForEach(gauges) { gauge in
                    gaugeView(gauge)
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
        .padding(.vertical, 6)
        .background(RoundedRectangle(cornerRadius: 8).fill(Color.white.opacity(0.05)))
    }

    private func gaugeView(_ gauge: Gauge) -> some View {
        HStack(spacing: 4) {
            Text(gauge.id)
                .font(.system(size: 8, weight: .semibold, design: .monospaced))
                .foregroundColor(.white.opacity(0.4))
            Capsule(style: .continuous)
                .fill(Color.white.opacity(0.1))
                .frame(width: 44, height: 3)
                .overlay(alignment: .leading) {
                    Capsule(style: .continuous)
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
            let segmentCount = SevenDayReadout.segmentCount
            let spacing = SevenDayReadout.segmentSpacing
            let totalSpacing = spacing * CGFloat(segmentCount - 1)
            let segmentWidth = max(1, (geo.size.width - totalSpacing) / CGFloat(segmentCount))
            let filledSegments = SevenDayReadout.litSegments(fraction, count: segmentCount)

            HStack(spacing: spacing) {
                ForEach(0..<segmentCount, id: \.self) { index in
                    RoundedRectangle(cornerRadius: 2)
                        .fill(index < filledSegments ? fillColor : emptyColor)
                        .frame(width: segmentWidth)
                }
            }
        }
    }
}

/// Fable's remaining under the 7d bar, as a square bracket on the same
/// scale: `└────┘` from the start of the track to the end of the last
/// segment Fable would light. A range annotation (the square bracket of
/// d3-annotation, a dimension line in a technical drawing), so it marks a
/// span without covering a single segment of the 7d fill, and it reads in
/// both directions — shorter or longer than the fill. No Fable left draws
/// only the start tick.
private struct FableBracket: View {
    let remaining: Double
    let color: Color

    /// Space between the bar and the bracket's ticks.
    static let gap: CGFloat = 2
    /// Tick length; the bracket's whole height.
    static let height: CGFloat = 3.5
    static let lineWidth: CGFloat = 1

    var body: some View {
        GeometryReader { geo in
            let end = SevenDayReadout.bracketEnd(remaining, width: geo.size.width)
            Path { path in
                let inset = Self.lineWidth / 2
                let bottom = geo.size.height - inset
                path.move(to: CGPoint(x: inset, y: 0))
                path.addLine(to: CGPoint(x: inset, y: bottom))
                if end > Self.lineWidth {
                    path.addLine(to: CGPoint(x: end - inset, y: bottom))
                    path.addLine(to: CGPoint(x: end - inset, y: 0))
                }
            }
            .stroke(color.opacity(0.9), style: StrokeStyle(lineWidth: Self.lineWidth, lineCap: .round, lineJoin: .round))
        }
        .accessibilityHidden(true)
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

/// The compact account row reads its gauges from the daemon's usage DTO.
extension CLIUsageInfo: CompactUsageInput {}
