import Foundation

/// DTOs decoded from the llmux daemon's HTTP API. The `accounts[]` slice of
/// `GET /llmux/status` is the read contract; it is identical in `/llmux/dashboard`.
struct LlmuxStatus: Decodable {
    let accounts: [LlmuxAccountRecord]
    let current: String?
    let port: Int?
    let version: String?
}

struct LlmuxAccountRecord: Decodable {
    let name: String
    let type: String            // "oauth" | "apikey" | "codex"
    let group: String?          // "claude" | "codex"
    let status: String?         // "active" | "ok" | "cooldown" | "auth_failed"
    let fiveHour: LlmuxWindow?
    let sevenDay: LlmuxWindow?
    let inFlight: Int?
    let tokenExpiresAtMs: UInt64?

    enum CodingKeys: String, CodingKey {
        case name, type, group, status
        case fiveHour = "five_hour"
        case sevenDay = "seven_day"
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

struct LoginStartResponse: Decodable {
    let state: String
    let provider: String?
}

struct LoginStatusResponse: Decodable {
    let phase: String            // "pending" | "done" | "error"
    let account: String?
    let error: String?
}
