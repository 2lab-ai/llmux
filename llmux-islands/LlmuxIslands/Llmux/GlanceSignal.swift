import Foundation

// exception-beacon (trinity 3-engine consensus, unanimous R5): the closed
// island stays PIXEL-IDENTICAL to today when everything is healthy, and shows
// at most ONE worst-exception text chip otherwise. Opening the island surfaces
// the SAME resolver's picks as a "NEEDS ATTENTION" list, so the chip and the
// panel can never disagree.
//
// Foundation-only on purpose: the resolver is pure so the LlmuxIslandsTests
// logic target exercises every priority/threshold/tie-break rule directly.

/// The single signal the closed label may show. Priority (worst first):
/// offline > auth > limit > lowQuota > degraded > none.
enum GlanceSignal: Equatable {
    /// The daemon is unreachable — NOT the same as "no sessions": the label
    /// hides the (stale) session counts and says so.
    case offline
    /// `count` accounts report `auth_failed` — re-login.
    case auth(count: Int)
    /// `count` accounts have a window at ACTUAL 0% remaining — rotate/wait.
    /// Distinct from `lowQuota`: exhaustion changes the action (now), an
    /// approaching limit changes the plan (soon).
    case limit(count: Int)
    /// The worst account×window at ≤10% remaining (but not exhausted):
    /// window is "5h" or "7d", remainingPct the ACTUAL remaining percent.
    case lowQuota(window: String, remainingPct: Int, account: String)
    /// An account the daemon EXPLICITLY reports as degraded, debounced over
    /// [`GlanceResolver.degradedDebounce`] consecutive polls. unknown/cold are
    /// never promoted here (no action to instruct → no beacon).
    case degraded
    case none

    /// The closed-label chip text. `nil` = render today's label unchanged.
    /// Text IS the signal (color is reinforcement only); quota always carries
    /// its window so 5h and 7d can never be confused.
    var chipText: String? {
        switch self {
        case .offline: return "offline"
        case .auth(let count): return "AUTH \(count)"
        case .limit(let count): return "LIMIT \(count)"
        case .lowQuota(let window, let remainingPct, _): return "\(window) \(remainingPct)%"
        case .degraded: return "degraded"
        case .none: return nil
        }
    }
}

/// One open-panel "NEEDS ATTENTION" row: the account, why, and what happens
/// next. Derived by the SAME resolver run as the closed chip.
struct GlanceAttention: Equatable, Identifiable {
    let account: String
    /// "authentication required — re-login", "5h limit exhausted", "5h 8%".
    let reason: String
    /// "resets in 1h 42m" when the window carries a reset. nil otherwise.
    let detail: String?
    var id: String { account + "·" + reason }
}

enum GlanceResolver {
    /// lowQuota entry threshold: remaining ≤ 10% of the window.
    static let lowQuotaRemaining = 0.10
    /// A window at ≥ this utilization is ACTUALLY exhausted (0% remaining
    /// after rounding) — the only thing `LIMIT` is allowed to mean.
    static let exhaustedUtilization = 0.995
    /// Consecutive polls an explicit degraded state must persist before the
    /// closed chip shows it. Release is IMMEDIATE (asymmetric on purpose:
    /// slow to alarm, instant to clear — no stale warnings).
    static let degradedDebounce = 3

    struct Output: Equatable {
        let signal: GlanceSignal
        let attention: [GlanceAttention]
    }

    /// Resolve the beacon + attention list from one poll's account records.
    /// `displayName` maps a record index/name to what the UI may show
    /// (demo-mode fakes); pass identity when no masking applies.
    static func resolve(
        records: [LlmuxAccountRecord],
        offline: Bool,
        degradedStreak: Int,
        displayName: (Int, String) -> String = { _, name in name }
    ) -> Output {
        if offline {
            return Output(signal: .offline, attention: [])
        }

        var attention: [GlanceAttention] = []

        // 1. auth_failed accounts (stable order: record order).
        var authCount = 0
        for (index, record) in records.enumerated() where record.status == "auth_failed" {
            authCount += 1
            attention.append(
                GlanceAttention(
                    account: displayName(index, record.name),
                    reason: "authentication required — re-login",
                    detail: nil
                )
            )
        }

        // 2/3. Windows: exhausted (LIMIT) and approaching (lowQuota).
        // Candidates over EVERY eligible account × {5h, 7d} — never an
        // average, never a representative account.
        var windows: [WindowState] = []
        for (index, record) in records.enumerated() where record.status != "auth_failed" {
            if let five = record.fiveHour {
                windows.append(WindowState(
                    index: index, name: record.name, window: "5h",
                    utilization: five.utilization, resetsInSecs: five.resetsInSecs
                ))
            }
            if let seven = record.sevenDay {
                windows.append(WindowState(
                    index: index, name: record.name, window: "7d",
                    utilization: seven.utilization, resetsInSecs: seven.resetsInSecs
                ))
            }
        }

        let exhaustedWindows = windows
            .filter { $0.utilization >= exhaustedUtilization }
            .sorted(by: Self.worstFirst)
        // LIMIT counts AFFECTED ACCOUNTS, not windows: one account with both
        // 5h and 7d at 0% is still "LIMIT 1".
        let limitAccounts = Set(exhaustedWindows.map(\.name))
        for state in exhaustedWindows {
            attention.append(
                GlanceAttention(
                    account: displayName(state.index, state.name),
                    reason: "\(state.window) limit exhausted",
                    detail: state.resetsInSecs.map { "resets in \(Self.duration($0))" }
                )
            )
        }

        let lowWindows = windows
            .filter { $0.utilization < exhaustedUtilization }
            .filter { 1.0 - $0.utilization <= lowQuotaRemaining }
            .filter { !limitAccounts.contains($0.name) }
            .sorted(by: Self.worstFirst)
        for state in lowWindows {
            attention.append(
                GlanceAttention(
                    account: displayName(state.index, state.name),
                    reason: "\(state.window) \(remainingPct(state.utilization))% left",
                    detail: state.resetsInSecs.map { "resets in \(Self.duration($0))" }
                )
            )
        }

        // 5. Explicit degraded (never inferred from unknown/cold): listed in
        // the open panel immediately; the CLOSED chip additionally waits for
        // the debounce.
        let degradedRecords = records.enumerated().filter { $0.element.status == "degraded" }
        for (index, record) in degradedRecords {
            attention.append(
                GlanceAttention(
                    account: displayName(index, record.name),
                    reason: "degraded",
                    detail: nil
                )
            )
        }

        let signal: GlanceSignal
        if authCount > 0 {
            signal = .auth(count: authCount)
        } else if !limitAccounts.isEmpty {
            signal = .limit(count: limitAccounts.count)
        } else if let worst = lowWindows.first {
            signal = .lowQuota(
                window: worst.window,
                remainingPct: remainingPct(worst.utilization),
                account: displayName(worst.index, worst.name)
            )
        } else if !degradedRecords.isEmpty, degradedStreak >= degradedDebounce {
            signal = .degraded
        } else {
            signal = .none
        }
        return Output(signal: signal, attention: attention)
    }

    /// One account×window candidate the resolver ranks.
    private struct WindowState {
        let index: Int
        let name: String
        let window: String
        let utilization: Double
        let resetsInSecs: Int?
    }

    /// Stable worst-first order for window candidates: lower remaining first,
    /// then 5h before 7d, then account name — so the beacon target can never
    /// flap between equal candidates across polls.
    private static func worstFirst(_ a: WindowState, _ b: WindowState) -> Bool {
        if a.utilization != b.utilization { return a.utilization > b.utilization }
        if a.window != b.window { return a.window == "5h" }
        return a.name < b.name
    }

    static func remainingPct(_ utilization: Double) -> Int {
        max(0, Int(((1.0 - utilization) * 100).rounded()))
    }

    /// "1h 42m" / "42m" / "50s" — compact reset countdown.
    static func duration(_ secs: Int) -> String {
        let secs = max(0, secs)
        if secs < 60 { return "\(secs)s" }
        let minutes = secs / 60
        if minutes < 60 { return "\(minutes)m" }
        let hours = minutes / 60
        let rem = minutes % 60
        if hours < 48 { return rem > 0 ? "\(hours)h \(rem)m" : "\(hours)h" }
        return "\(hours / 24)d \(hours % 24)h"
    }
}
