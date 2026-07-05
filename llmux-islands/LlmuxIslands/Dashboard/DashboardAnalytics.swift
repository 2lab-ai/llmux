import Foundation

// Pure (Foundation-only) analytics logic behind the Islands dashboard UI
// (issue #62 S4): formatting, the health-warning rule, data-quality label
// resolution, and the (group, model) row identity. Kept free of SwiftUI so the
// LlmuxIslandsTests logic target can exercise every v1 hard rule directly.

/// Fallback wording for `dashboard.data_quality` when the daemon predates the
/// field. BYTE-IDENTICAL to the Rust serde defaults in `src/dashboard.rs`
/// (slice S2) — the server field is the SSOT, these only cover old daemons.
enum DataQualityFallback {
    static let modelUsage = "hydrated activity/runtime"
    static let windowed = "best effort"
    static let cost = "API-equivalent estimate"
    static let cache = "missing fields shown as unavailable"
}

/// Resolved data-quality labels: the server-provided string when present,
/// else the byte-identical fallback constant (U19–U22).
struct DataQualityLabels {
    let modelUsage: String
    let windowed: String
    let cost: String
    let cache: String

    init(_ quality: LlmuxDashboardDataQuality?) {
        modelUsage = quality?.modelUsage ?? DataQualityFallback.modelUsage
        windowed = quality?.windowed ?? DataQualityFallback.windowed
        cost = quality?.cost ?? DataQualityFallback.cost
        cache = quality?.cache ?? DataQualityFallback.cache
    }
}

/// Display formatting shared by every analytics surface. Core rule: a missing
/// (nil) value renders as `—` (unavailable), NEVER as a fabricated 0.
enum DashFormat {
    static let unavailable = "—"

    /// Compact count: 999 → "999", 12345 → "12.3k", 109613759 → "110M".
    static func count(_ value: UInt64) -> String {
        switch value {
        case ..<1_000: return "\(value)"
        case ..<1_000_000: return scaled(Double(value) / 1_000, "k")
        case ..<1_000_000_000: return scaled(Double(value) / 1_000_000, "M")
        default: return scaled(Double(value) / 1_000_000_000, "B")
        }
    }

    /// nil → `—` (cache counters etc. — U21), value → compact count.
    static func count(_ value: UInt64?) -> String {
        value.map { count($0) } ?? unavailable
    }

    /// nil → `—`, never $0.00 for an absent value (old daemon). 0.42 → "$0.42".
    static func cost(_ value: Double?) -> String {
        guard let value else { return unavailable }
        if value < 10 { return String(format: "$%.2f", value) }
        if value < 1_000 { return String(format: "$%.0f", value) }
        return "$" + count(UInt64(value.rounded()))
    }

    /// Client-usage cost: S1 emits wire-ready ZEROS until per-client
    /// attribution lands (#32) — 0 means "not attributed", not "free". Issue
    /// #68: absent data is an OMITTED element (nil), never a `—` column.
    static func clientCost(_ value: Double?) -> String? {
        guard let value, value > 0 else { return nil }
        return cost(value)
    }

    /// Client-usage last-seen: 0/absent → omitted (nil), never a 1970 date.
    static func clientLastSeen(_ ms: UInt64?, now: Date) -> String? {
        guard let ms, ms > 0 else { return nil }
        return ago(ms: ms, now: now)
    }

    /// errors / max(requests, 1) — U7 error-rate definition.
    static func errorRate(errors: UInt64, requests: UInt64) -> Double {
        Double(errors) / Double(max(requests, 1))
    }

    /// 0.02763 → "2.8%".
    static func percent(_ rate: Double) -> String {
        String(format: "%.1f%%", rate * 100)
    }

    /// Time since an epoch-ms instant: "42s" / "5m" / "3h" / "2d".
    static func ago(ms: UInt64, now: Date) -> String {
        let seconds = max(0, Int(now.timeIntervalSince1970 - Double(ms) / 1000))
        if seconds < 60 { return "\(seconds)s" }
        if seconds < 3_600 { return "\(seconds / 60)m" }
        if seconds < 86_400 { return "\(seconds / 3_600)h" }
        return "\(seconds / 86_400)d"
    }

    /// Time until an epoch-ms instant (token expiry): "in 5m", or "expired"
    /// once past — a real state, not a `—` placeholder (#68).
    static func until(ms: UInt64, now: Date) -> String {
        let seconds = Int(Double(ms) / 1000 - now.timeIntervalSince1970)
        guard seconds > 0 else { return "expired" }
        if seconds < 60 { return "in \(seconds)s" }
        if seconds < 3_600 { return "in \(seconds / 60)m" }
        if seconds < 86_400 { return "in \(seconds / 3_600)h" }
        return "in \(seconds / 86_400)d"
    }

    /// Request duration: 320 → "320ms", 9752 → "9.8s".
    static func duration(ms: UInt64) -> String {
        ms < 1_000 ? "\(ms)ms" : String(format: "%.1fs", Double(ms) / 1000)
    }

    private static func scaled(_ value: Double, _ suffix: String) -> String {
        value >= 100
            ? String(format: "%.0f%@", value, suffix)
            : String(format: "%.1f%@", value, suffix)
    }
}

/// The Statistics panel's section set (issue #68 v2). The four #62 analytics
/// surfaces live behind the ☰ menu's "Statistics" entry — pure (Foundation
/// only) so the logic tests pin the contract: four sections, this order,
/// these titles.
enum StatsSection: String, CaseIterable {
    case overview
    case models
    case clients
    case health

    /// Segment label; English, matching the app's menu-item language.
    var title: String {
        switch self {
        case .overview: return "Overview"
        case .models: return "Models"
        case .clients: return "Clients"
        case .health: return "Health"
        }
    }
}

/// The U11/U13 health-warning rule — ONE source for the banner and the
/// closed-island warning color: any account `status == "auth_failed"` OR any
/// quota > 90%. "Quota" = the account-wide 5h/7d windows; the scoped Fable
/// weekly window counts only while the daemon marks it `constraining` (it is
/// scoped to Fable models, and `constraining` is the reset-aware bool the
/// tiles already key red off — a stale 91% that constrains nothing must not
/// paint the island red).
enum DashboardHealth {
    static let quotaThreshold = 0.9

    struct Summary: Equatable {
        var authFailed = 0
        var overQuota = 0
        var isWarning: Bool { authFailed > 0 || overQuota > 0 }
        /// Accounts in ANY warning state — the closed pill's `⚠N` count.
        var total: Int { authFailed + overQuota }
    }

    static func isOverQuota(
        fiveHour: Double?, sevenDay: Double?,
        fableUtilization: Double?, fableConstraining: Bool?
    ) -> Bool {
        if let fiveHour, fiveHour > quotaThreshold { return true }
        if let sevenDay, sevenDay > quotaThreshold { return true }
        if let fableUtilization, fableUtilization > quotaThreshold, fableConstraining == true {
            return true
        }
        return false
    }

    /// Dashboard-path accounts (`/llmux/dashboard`).
    static func summary(_ accounts: [LlmuxDashboardAccount]) -> Summary {
        var result = Summary()
        for account in accounts {
            if account.status == "auth_failed" {
                result.authFailed += 1
            } else if isOverQuota(
                fiveHour: account.fiveHour?.utilization,
                sevenDay: account.sevenDay?.utilization,
                fableUtilization: account.fableWeekly?.utilization,
                fableConstraining: account.fableWeekly?.constraining
            ) {
                result.overQuota += 1
            }
        }
        return result
    }

    /// Status-fallback accounts (`/llmux/status`, old daemons) — same rule, so
    /// the closed-island warning color works on both poll paths.
    static func summary(records: [LlmuxAccountRecord]) -> Summary {
        var result = Summary()
        for record in records {
            if record.status == "auth_failed" {
                result.authFailed += 1
            } else if isOverQuota(
                fiveHour: record.fiveHour?.utilization,
                sevenDay: record.sevenDay?.utilization,
                fableUtilization: record.fableWeekly?.utilization,
                fableConstraining: record.fableWeekly?.constraining
            ) {
                result.overQuota += 1
            }
        }
        return result
    }

    /// Banner wording: "1 account auth failed · 2 accounts over 90% quota".
    static func bannerText(_ summary: Summary) -> String {
        var parts: [String] = []
        if summary.authFailed > 0 {
            parts.append("\(summary.authFailed) account\(summary.authFailed == 1 ? "" : "s") auth failed")
        }
        if summary.overQuota > 0 {
            parts.append("\(summary.overQuota) account\(summary.overQuota == 1 ? "" : "s") over 90% quota")
        }
        return parts.joined(separator: " · ")
    }
}

/// Human display label for a `client_usage[].client` id (issue #68).
///
/// Claude Code sends `metadata.user_id` as a JSON blob
/// `{"device_id":"<64 hex>","account_uuid":"<uuid>","session_id":"<uuid>"}`
/// (shape verified against a live daemon's `/llmux/dashboard`, 2026-07-05);
/// rendering it raw put braces, quotes and truncated JSON in the Clients tab.
/// That shape parses into `<first8>…<last4> · sess <first4>` — e.g.
/// `eb5df6d4…8aea · sess 468a`. Anything that is NOT a JSON object carrying a
/// string `device_id` (plain `unknown`, API-key client names, future formats,
/// garbage) renders verbatim.
enum ClientIDLabel {
    static func display(_ raw: String) -> String {
        guard let object = try? JSONSerialization.jsonObject(with: Data(raw.utf8)) as? [String: Any],
              let deviceID = (object["device_id"] as? String)?
                  .trimmingCharacters(in: .whitespacesAndNewlines),
              !deviceID.isEmpty
        else { return raw }

        var label = shortHash(deviceID)
        if let sessionID = (object["session_id"] as? String)?
            .trimmingCharacters(in: .whitespacesAndNewlines),
            !sessionID.isEmpty {
            label += " · sess \(String(sessionID.prefix(4)))"
        }
        return label
    }

    /// `eb5df6d4…8aea` — first 8 + last 4. Ids too short to compress (≤ 12
    /// chars, where the shortening would not shorten) render whole.
    private static func shortHash(_ id: String) -> String {
        guard id.count > 12 else { return id }
        return "\(id.prefix(8))…\(id.suffix(4))"
    }
}

/// Closed-island pill content (issue #68): `llmux [⚠N] [C{n}] [X{m}] [· $x]`.
/// ONE source for the pill text: the live view styles these segments and the
/// tests assert `text`. Zero in-flight counters, a zero warning count and an
/// absent cost are OMITTED — never rendered as `C:0`/`X:0` noise. Idle
/// example: `llmux ⚠5 · $9.1k`.
struct ClosedPillSegments: Equatable {
    static let prefix = "llmux"

    /// `⚠N` badge count; nil (healthy) hides the badge.
    let warningCount: Int?
    /// `C{n}` in-flight Claude sessions; nil (zero) hides the segment.
    let claude: Int?
    /// `X{m}` in-flight Codex sessions; nil (zero) hides the segment.
    let codex: Int?
    /// Formatted `totals.cost_usd`; nil (old daemon) omits the segment —
    /// never a fabricated $0.00. A REAL server 0 still renders ($0.00).
    let cost: String?

    init(claude: Int, codex: Int, warningCount: Int, costUsd: Double?) {
        self.warningCount = warningCount > 0 ? warningCount : nil
        self.claude = claude > 0 ? claude : nil
        self.codex = codex > 0 ? codex : nil
        cost = costUsd.map { DashFormat.cost($0) }
    }

    /// Plain-string form of the whole pill (tests + accessibility).
    var text: String {
        var parts = [Self.prefix]
        if let warningCount { parts.append("⚠\(warningCount)") }
        if let claude { parts.append("C\(claude)") }
        if let codex { parts.append("X\(codex)") }
        var joined = parts.joined(separator: " ")
        if let cost { joined += " · \(cost)" }
        return joined
    }
}

/// Row identity = `(group, model)` — the U13 hard rule. Claude and Codex rows
/// with the same model text stay separate; every ForEach over model rows MUST
/// key off this id, never `model` alone.
extension LlmuxDashboardModelUsage: Identifiable {
    var id: String { "\(group)/\(model)" }
}

extension LlmuxDashboardModelUsage {
    /// Top rows by total tokens (gist §4.3), (group, model) identity intact.
    /// Deterministic: ties break on the row id.
    static func top(_ rows: [LlmuxDashboardModelUsage], limit: Int = 3) -> [LlmuxDashboardModelUsage] {
        Array(
            rows.sorted {
                $0.totalTokens != $1.totalTokens ? $0.totalTokens > $1.totalTokens : $0.id < $1.id
            }
            .prefix(limit)
        )
    }
}

extension LlmuxDashboardClientUsage: Identifiable {
    var id: String { client }
}

extension LlmuxDashboardWindowedCell: Identifiable {
    var id: String { "\(group)/\(model)/\(account)" }
}
