import Foundation

// DTOs decoded from `GET /llmux/dashboard` (issue #62 slice S3). The dashboard
// document is a strict superset of `/llmux/status` — same `accounts[]` fields —
// plus totals / model_usage / client_usage / windowed / activity analytics.
//
// Wire-compat rule (mirrors `src/dashboard.rs`, which marks additive fields
// `#[serde(default)]`): every field the server added after the endpoint first
// shipped decodes as Optional or with a default here, so a doc from an OLDER
// daemon never fails to decode for a missing additive field. A decode failure
// is reserved for genuinely broken/foreign payloads — that is what triggers
// `IslandUsageModel`'s fallback to the `/llmux/status` path (gist-02 L33).
//
// Fields Islands does not consume (pid, upstream, select_params, scheduler,
// poller, logs, codex, per-account order/scoped_limits/totals/session) are
// deliberately not modeled — `Decodable` ignores unknown JSON keys, so they
// stay additive on the wire without a Swift-side change.

/// Top-level `GET /llmux/dashboard` document.
struct LlmuxDashboard: Decodable {
    let version: String
    let port: Int
    let uptimeSecs: UInt64
    /// Representative current account (claude slot first) — status parity.
    let current: String?
    /// Per-group sticky current account. Additive → empty for old daemons.
    let currentByGroup: [String: String]
    let accounts: [LlmuxDashboardAccount]
    let totals: LlmuxDashboardTotals
    /// Additive → `[]` for docs that predate model usage.
    let modelUsage: [LlmuxDashboardModelUsage]
    /// Additive → `[]` for docs that predate client attribution (issue #32).
    let clientUsage: [LlmuxDashboardClientUsage]
    /// 24h/72h heatmap slices — BEST EFFORT, not a billing ledger (issue #23).
    /// Additive → `[]` for old daemons.
    let windowed: [LlmuxDashboardWindowed]
    let activity: LlmuxDashboardActivity
    /// Server-owned "Email anonymous" display setting. `nil` = daemon predates
    /// it → the app keeps its local-only toggle behavior (same as status E7).
    let emailAnonymous: Bool?
    /// Whether the daemon wants the Fable weekly gauge rendered
    /// (`show_fable_weekly` config). `nil` = old daemon → default ON.
    let showFableWeekly: Bool?
    /// Data-quality labels (issue #62 U18). The server field ships in a later
    /// slice — decoded fully optionally so its absence today is not an error.
    let dataQuality: LlmuxDashboardDataQuality?

    enum CodingKeys: String, CodingKey {
        case version, port, current, accounts, totals, windowed, activity
        case uptimeSecs = "uptime_secs"
        case currentByGroup = "current_by_group"
        case modelUsage = "model_usage"
        case clientUsage = "client_usage"
        case emailAnonymous = "email_anonymous"
        case showFableWeekly = "show_fable_weekly"
        case dataQuality = "data_quality"
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        version = try c.decode(String.self, forKey: .version)
        port = try c.decode(Int.self, forKey: .port)
        uptimeSecs = try c.decode(UInt64.self, forKey: .uptimeSecs)
        current = try c.decodeIfPresent(String.self, forKey: .current)
        currentByGroup = try c.decodeIfPresent([String: String].self, forKey: .currentByGroup) ?? [:]
        accounts = try c.decode([LlmuxDashboardAccount].self, forKey: .accounts)
        totals = try c.decode(LlmuxDashboardTotals.self, forKey: .totals)
        modelUsage = try c.decodeIfPresent([LlmuxDashboardModelUsage].self, forKey: .modelUsage) ?? []
        clientUsage = try c.decodeIfPresent([LlmuxDashboardClientUsage].self, forKey: .clientUsage) ?? []
        windowed = try c.decodeIfPresent([LlmuxDashboardWindowed].self, forKey: .windowed) ?? []
        activity = try c.decode(LlmuxDashboardActivity.self, forKey: .activity)
        emailAnonymous = try c.decodeIfPresent(Bool.self, forKey: .emailAnonymous)
        showFableWeekly = try c.decodeIfPresent(Bool.self, forKey: .showFableWeekly)
        dataQuality = try c.decodeIfPresent(LlmuxDashboardDataQuality.self, forKey: .dataQuality)
    }
}

/// One `accounts[]` entry — the same wire object `/llmux/status` serves (minus
/// `group`, which the dashboard doc does not carry; the provider split falls
/// back to `type == "codex"`, which is exactly how the daemon derives group).
struct LlmuxDashboardAccount: Decodable {
    let name: String
    let type: String            // "oauth" | "apikey" | "codex"
    let status: String?         // "active" | "ok" | "cooldown" | "auth_failed"
    let blocked: String?
    let healthy: Bool?
    let fiveHour: LlmuxDashboardWindow?
    let sevenDay: LlmuxDashboardWindow?
    /// Fable weekly window — reuses the status DTO (identical wire object) so
    /// the tile mapping stays byte-for-byte the same on both paths.
    let fableWeekly: LlmuxScopedWindow?
    let cooldownUntil: UInt64?
    let cooldownSource: String?
    let inFlight: Int?
    let tokenExpiresAtMs: UInt64?
    let lastRefreshMs: UInt64?

    enum CodingKeys: String, CodingKey {
        case name, type, status, blocked, healthy
        case fiveHour = "five_hour"
        case sevenDay = "seven_day"
        case fableWeekly = "fable_weekly"
        case cooldownUntil = "cooldown_until"
        case cooldownSource = "cooldown_source"
        case inFlight = "in_flight"
        case tokenExpiresAtMs = "token_expires_at_ms"
        case lastRefreshMs = "last_refresh_ms"
    }
}

/// A 5h/7d quota window (dashboard form — carries freshness fields the status
/// window omits).
struct LlmuxDashboardWindow: Decodable {
    let utilization: Double      // 0...1
    let resetsAt: UInt64?        // epoch seconds
    let resetsInSecs: Int?
    let fetchedAtMs: UInt64?
    let source: String?          // "headers" | "poll"

    enum CodingKeys: String, CodingKey {
        case utilization, source
        case resetsAt = "resets_at"
        case resetsInSecs = "resets_in_secs"
        case fetchedAtMs = "fetched_at_ms"
    }
}

/// Global KPI totals.
struct LlmuxDashboardTotals: Decodable {
    let requests: UInt64
    let ok: UInt64
    let errors: UInt64
    let tokensIn: UInt64
    let tokensOut: UInt64
    let rpm5m: Double?
    let inFlight: Int?
    /// API-equivalent USD estimate. Additive → `nil` for old daemons; render
    /// `nil` as unavailable (`—`), never as 0 (gist data-quality rule).
    let costUsd: Double?

    var totalTokens: UInt64 { tokensIn &+ tokensOut }

    enum CodingKeys: String, CodingKey {
        case requests, ok, errors
        case tokensIn = "tokens_in"
        case tokensOut = "tokens_out"
        case rpm5m = "rpm_5m"
        case inFlight = "in_flight"
        case costUsd = "cost_usd"
    }
}

/// One per-(group, model) usage row. The row key is `(group, model)` — never
/// merge claude/codex rows on model text alone.
struct LlmuxDashboardModelUsage: Decodable {
    let group: String            // "claude" | "codex"
    let model: String
    let requests: UInt64
    let ok: UInt64
    let errors: UInt64
    let tokensIn: UInt64
    let tokensOut: UInt64
    /// `nil` = upstream did not report cache counters (render `—`, not 0).
    let cacheRead: UInt64?
    let cacheCreation: UInt64?
    let lastUsedMs: UInt64
    let inFlight: Int            // additive → 0
    let accounts: [LlmuxDashboardModelAccount]   // additive → []
    let efforts: [LlmuxDashboardModelCount]      // additive → []
    let endpoints: [LlmuxDashboardModelCount]    // additive → []
    /// Server-computed API-equivalent cost (issue #62 S1, additive). `nil` =
    /// daemon predates it — Islands must not re-derive pricing locally.
    let costUsd: Double?

    var totalTokens: UInt64 {
        tokensIn &+ tokensOut &+ (cacheRead ?? 0) &+ (cacheCreation ?? 0)
    }

    enum CodingKeys: String, CodingKey {
        case group, model, requests, ok, errors, accounts, efforts, endpoints
        case tokensIn = "tokens_in"
        case tokensOut = "tokens_out"
        case cacheRead = "cache_read"
        case cacheCreation = "cache_creation"
        case lastUsedMs = "last_used_ms"
        case inFlight = "in_flight"
        case costUsd = "cost_usd"
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        group = try c.decode(String.self, forKey: .group)
        model = try c.decode(String.self, forKey: .model)
        requests = try c.decode(UInt64.self, forKey: .requests)
        ok = try c.decode(UInt64.self, forKey: .ok)
        errors = try c.decode(UInt64.self, forKey: .errors)
        tokensIn = try c.decode(UInt64.self, forKey: .tokensIn)
        tokensOut = try c.decode(UInt64.self, forKey: .tokensOut)
        cacheRead = try c.decodeIfPresent(UInt64.self, forKey: .cacheRead)
        cacheCreation = try c.decodeIfPresent(UInt64.self, forKey: .cacheCreation)
        lastUsedMs = try c.decode(UInt64.self, forKey: .lastUsedMs)
        inFlight = try c.decodeIfPresent(Int.self, forKey: .inFlight) ?? 0
        accounts = try c.decodeIfPresent([LlmuxDashboardModelAccount].self, forKey: .accounts) ?? []
        efforts = try c.decodeIfPresent([LlmuxDashboardModelCount].self, forKey: .efforts) ?? []
        endpoints = try c.decodeIfPresent([LlmuxDashboardModelCount].self, forKey: .endpoints) ?? []
        costUsd = try c.decodeIfPresent(Double.self, forKey: .costUsd)
    }
}

/// Per-account contribution inside one model row.
struct LlmuxDashboardModelAccount: Decodable {
    let name: String
    let requests: UInt64
    let ok: UInt64
    let errors: UInt64
    let tokensIn: UInt64
    let tokensOut: UInt64

    enum CodingKeys: String, CodingKey {
        case name, requests, ok, errors
        case tokensIn = "tokens_in"
        case tokensOut = "tokens_out"
    }
}

/// A labelled request count (an effort level or an endpoint class).
struct LlmuxDashboardModelCount: Decodable {
    let label: String
    let requests: UInt64
}

/// One per-client attribution row (`metadata.user_id`, or `unknown`).
struct LlmuxDashboardClientUsage: Decodable {
    let client: String
    let requests: UInt64
    let ok: UInt64
    let errors: UInt64
    let tokensIn: UInt64
    let tokensOut: UInt64
    /// Additive (issue #62 S1) → `nil` for daemons that predate them.
    let costUsd: Double?
    let lastSeenMs: UInt64?

    enum CodingKeys: String, CodingKey {
        case client, requests, ok, errors
        case tokensIn = "tokens_in"
        case tokensOut = "tokens_out"
        case costUsd = "cost_usd"
        case lastSeenMs = "last_seen_ms"
    }
}

/// One trailing-window heatmap slice ("24h" / "72h"). Best-effort sample.
struct LlmuxDashboardWindowed: Decodable {
    let window: String
    let windowSecs: UInt64
    let cells: [LlmuxDashboardWindowedCell]

    enum CodingKeys: String, CodingKey {
        case window, cells
        case windowSecs = "window_secs"
    }
}

/// One `(group, model, account)` heatmap cell.
struct LlmuxDashboardWindowedCell: Decodable {
    let group: String
    let model: String
    let account: String
    let requests: UInt64
    let ok: UInt64
    let errors: UInt64
    let tokensIn: UInt64
    let tokensOut: UInt64
    let cacheRead: UInt64        // additive → 0
    let cacheCreation: UInt64    // additive → 0
    /// Combined intensity (in + out + cache), server-computed.
    let tokens: UInt64

    enum CodingKeys: String, CodingKey {
        case group, model, account, requests, ok, errors, tokens
        case tokensIn = "tokens_in"
        case tokensOut = "tokens_out"
        case cacheRead = "cache_read"
        case cacheCreation = "cache_creation"
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        group = try c.decode(String.self, forKey: .group)
        model = try c.decode(String.self, forKey: .model)
        account = try c.decode(String.self, forKey: .account)
        requests = try c.decode(UInt64.self, forKey: .requests)
        ok = try c.decode(UInt64.self, forKey: .ok)
        errors = try c.decode(UInt64.self, forKey: .errors)
        tokensIn = try c.decode(UInt64.self, forKey: .tokensIn)
        tokensOut = try c.decode(UInt64.self, forKey: .tokensOut)
        cacheRead = try c.decodeIfPresent(UInt64.self, forKey: .cacheRead) ?? 0
        cacheCreation = try c.decodeIfPresent(UInt64.self, forKey: .cacheCreation) ?? 0
        tokens = try c.decode(UInt64.self, forKey: .tokens)
    }
}

/// Real-time activity tail: in-flight + recent completed requests.
struct LlmuxDashboardActivity: Decodable {
    let inFlight: [LlmuxDashboardInFlight]
    let completed: [LlmuxDashboardCompleted]

    enum CodingKeys: String, CodingKey {
        case inFlight = "in_flight"
        case completed
    }
}

/// One started-but-unfinished request.
struct LlmuxDashboardInFlight: Decodable {
    let id: UInt64
    let method: String
    let path: String
    let account: String?
    let startedAtMs: UInt64
    let group: String?           // additive
    let model: String?           // additive

    enum CodingKeys: String, CodingKey {
        case id, method, path, account, group, model
        case startedAtMs = "started_at_ms"
    }
}

/// One completed-activity entry. On the wire this is a `kind`-tagged enum
/// (`"request"` | `"note"` — see `CompletedDoc` in `src/dashboard.rs`), so
/// everything variant-specific is optional: a `note` row has `text`/`error`,
/// a `request` row has the HTTP fields. Unknown future kinds still decode
/// (only `kind` + `at_ms` are common), keeping the doc additive.
struct LlmuxDashboardCompleted: Decodable {
    let kind: String             // "request" | "note"
    let atMs: UInt64

    // "request" fields
    let method: String?
    let path: String?
    let account: String?
    let status: Int?
    let durationMs: UInt64?
    let tokens: Tokens?
    let costUsd: Double?         // additive
    let group: String?           // additive
    let model: String?           // additive
    let effort: String?          // additive

    // "note" fields
    let text: String?
    let error: Bool?

    var isRequest: Bool { kind == "request" }
    var isNote: Bool { kind == "note" }

    struct Tokens: Decodable {
        let input: UInt64
        let output: UInt64
    }

    enum CodingKeys: String, CodingKey {
        case kind, method, path, account, status, tokens, group, model, effort, text, error
        case atMs = "at_ms"
        case durationMs = "duration_ms"
        case costUsd = "cost_usd"
    }
}

/// Data-quality labels (issue #62 U18): server-owned wording for how each
/// analytics section should be qualified. The wire field ships in a later
/// slice, so every key decodes optionally.
struct LlmuxDashboardDataQuality: Decodable {
    let modelUsage: String?      // e.g. "hydrated activity/runtime"
    let windowed: String?        // e.g. "best effort"
    let cost: String?            // e.g. "API-equivalent estimate"
    let cache: String?           // e.g. "missing fields shown as unavailable"

    enum CodingKeys: String, CodingKey {
        case windowed, cost, cache
        case modelUsage = "model_usage"
    }
}

extension LlmuxDashboardAccount {
    /// Bridge to the `/llmux/status` account record so the dashboard path
    /// feeds the EXACT same tile mapping (`IslandUsageModel.tile(from:)`) as
    /// the status path — tile parity by construction, not by duplication.
    /// `group` is `nil` because the dashboard doc omits it; the provider split
    /// then falls back to `type == "codex"`, which matches how the daemon
    /// derives the group from the credential kind (`src/scheduler/mod.rs`).
    var statusRecord: LlmuxAccountRecord {
        LlmuxAccountRecord(
            name: name,
            type: type,
            group: nil,
            status: status,
            fiveHour: fiveHour.map { LlmuxWindow(utilization: $0.utilization, resetsInSecs: $0.resetsInSecs) },
            sevenDay: sevenDay.map { LlmuxWindow(utilization: $0.utilization, resetsInSecs: $0.resetsInSecs) },
            fableWeekly: fableWeekly,
            inFlight: inFlight,
            tokenExpiresAtMs: tokenExpiresAtMs
        )
    }
}
