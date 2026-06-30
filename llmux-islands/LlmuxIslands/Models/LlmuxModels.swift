import Foundation

/// Backend provider family — drives the tile icon + grouping. llmux groups
/// Anthropic accounts (oauth + apikey) under "claude" and ChatGPT under "codex".
enum LlmuxProvider: String, Codable, Hashable {
    case claude
    case codex
    case unknown

    init(group: String?, type: String) {
        switch (group ?? "").lowercased() {
        case "claude": self = .claude
        case "codex": self = .codex
        default:
            self = (type.lowercased() == "codex") ? .codex : .claude
        }
    }

    var displayName: String {
        switch self {
        case .claude: return "Claude"
        case .codex: return "Codex"
        case .unknown: return "LLM"
        }
    }
}

/// One 5h or 7d quota window (the `five_hour` / `seven_day` objects in
/// `/llmux/status`). `utilization` is 0...1; the resets fields are seconds and
/// epoch-seconds respectively. A null window decodes to `nil`.
struct LlmuxWindow: Decodable, Hashable {
    let utilization: Double
    let resetsInSecs: Int?
    let resetsAt: Int?

    enum CodingKeys: String, CodingKey {
        case utilization
        case resetsInSecs = "resets_in_secs"
        case resetsAt = "resets_at"
    }

    /// 0...100 for display.
    var percent: Int { Int((max(0, min(1, utilization)) * 100).rounded()) }
}

/// One account row from `GET /llmux/status` (the `accounts[]` slice is identical
/// in `/llmux/dashboard`). Lenient: only `name` + `type` are required so the app
/// tolerates schema drift, null windows, and apikey accounts (no token expiry).
struct LlmuxAccount: Decodable, Identifiable, Hashable {
    let name: String
    let type: String
    let group: String?
    let status: String?
    let fiveHour: LlmuxWindow?
    let sevenDay: LlmuxWindow?
    let inFlight: Int?
    let tokenExpiresAtMs: UInt64?

    var id: String { name }
    var provider: LlmuxProvider { LlmuxProvider(group: group, type: type) }

    /// A short, human label: the email-ish part after the `claude:` / `codex:`
    /// prefix llmux assigns, or the raw name (e.g. `api-1`).
    var label: String {
        if let colon = name.firstIndex(of: ":") {
            return String(name[name.index(after: colon)...])
        }
        return name
    }

    enum CodingKeys: String, CodingKey {
        case name, type, group, status
        case fiveHour = "five_hour"
        case sevenDay = "seven_day"
        case inFlight = "in_flight"
        case tokenExpiresAtMs = "token_expires_at_ms"
    }
}

/// The subset of `GET /llmux/status` the app reads.
struct LlmuxStatus: Decodable {
    let accounts: [LlmuxAccount]
    let current: String?
    let port: Int?
    let version: String?
}

/// `POST /llmux/login/start` response.
struct LoginStartResponse: Decodable {
    let state: String
    let provider: String?
}

/// `GET /llmux/login/status` response.
struct LoginStatusResponse: Decodable {
    let phase: String
    let account: String?
    let error: String?
}
