import Foundation

/// DTOs decoded from the llmux daemon's HTTP API. The `accounts[]` slice of
/// `GET /llmux/status` is the read contract; it is identical in `/llmux/dashboard`.
struct LlmuxStatus: Decodable {
    let accounts: [LlmuxAccountRecord]
    let current: String?
    let port: Int?
    let version: String?
    /// Server-owned "Email anonymous" display setting (llmux 0.2.10+).
    /// Optional on purpose: an older daemon omits the field, and the app then
    /// falls back to its local-only toggle behavior (SSOT E7).
    let emailAnonymous: Bool?

    enum CodingKeys: String, CodingKey {
        case accounts, current, port, version
        case emailAnonymous = "email_anonymous"
    }
}

struct LlmuxAccountRecord: Decodable {
    let name: String
    let type: String            // "oauth" | "apikey" | "codex"
    let group: String?          // "claude" | "codex"
    let status: String?         // "active" | "ok" | "cooldown" | "auth_failed"
    let fiveHour: LlmuxWindow?
    let sevenDay: LlmuxWindow?
    /// The Fable weekly (7d) usage window. Optional on purpose: an OLD daemon
    /// omits `fable_weekly` entirely, and the daemon reports `null` for an
    /// account with no Fable weekly limit — both decode to `nil`. This window
    /// is temporary (the upstream limit is expected to disappear) so it carries
    /// its own severity/is_active rather than reusing `LlmuxWindow`.
    let fableWeekly: LlmuxScopedWindow?
    let inFlight: Int?
    let tokenExpiresAtMs: UInt64?

    enum CodingKeys: String, CodingKey {
        case name, type, group, status
        case fiveHour = "five_hour"
        case sevenDay = "seven_day"
        case fableWeekly = "fable_weekly"
        case inFlight = "in_flight"
        case tokenExpiresAtMs = "token_expires_at_ms"
    }
}

struct LlmuxWindow: Decodable {
    let utilization: Double      // 0...1
    let resetsInSecs: Int?

    enum CodingKeys: String, CodingKey {
        case utilization
        case resetsInSecs = "resets_in_secs"
    }
}

/// A usage window that also reports a server-computed `severity` and whether it
/// is the currently binding limit (`is_active`). Used by `fable_weekly`, which
/// the tile emphasizes in red when critical/active.
struct LlmuxScopedWindow: Decodable {
    let utilization: Double      // 0...1
    let resetsInSecs: Int?
    let resetsAt: Int?           // epoch seconds; informational
    let severity: String?        // e.g. "ok" | "warning" | "critical"
    let isActive: Bool?

    enum CodingKeys: String, CodingKey {
        case utilization
        case resetsInSecs = "resets_in_secs"
        case resetsAt = "resets_at"
        case severity
        case isActive = "is_active"
    }
}

struct LoginStartResponse: Decodable {
    let state: String
    let provider: String?
}

struct LoginStatusResponse: Decodable {
    let phase: String            // "pending" | "done" | "error"
    let account: String?
    let error: String?
}
