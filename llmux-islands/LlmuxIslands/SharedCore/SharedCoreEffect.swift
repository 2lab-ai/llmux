import Foundation

/// Transient executor instruction returned beside canonical UiState. Effects
/// may contain resolved raw daemon account ids, so callers must execute and
/// discard them; they are never published, persisted, rendered or logged.
struct SharedCoreEffect: Decodable {
    let type: String
    let requestID: String?
    let retryAtMs: UInt64?
    let operationID: String?
    let provider: String?
    let state: String?
    let emailAnonymous: Bool?
    let eventID: String?
    let enabled: Bool?
    let request: SharedCoreOperationRequest?
    let event: SharedCoreEvent?
    let change: SharedCoreLocalSettingsChange?
    let command: SharedCoreMaintenanceCommand?
    let providerInFlight: [String: UInt32]?

    enum CodingKeys: String, CodingKey {
        case type, provider, state, enabled, request, event, change, command
        case requestID = "request_id"
        case retryAtMs = "retry_at_ms"
        case operationID = "operation_id"
        case emailAnonymous = "email_anonymous"
        case eventID = "event_id"
        case providerInFlight = "provider_in_flight"
    }

    var dashboardRequestID: String? { type == "fetch_dashboard" ? requestID : nil }
}

struct SharedCoreOperationRequest: Decodable {
    let kind: String
    let name: String?
    let accountID: String?
    let paused: Bool?
    let confirmed: Bool?
    let apiKeyRequired: Bool?

    enum CodingKeys: String, CodingKey {
        case kind, name, paused, confirmed
        case accountID = "account_id"
        case apiKeyRequired = "api_key_required"
    }
}

struct SharedCoreEvent: Decodable {
    let id: String
    let from: String
    let to: String
    let content: String
}

struct SharedCoreLocalSettingsChange: Decodable {
    let kind: String
    let id: String?
    let enabled: Bool?
    let endpoint: String?
    let apiKeyConfigured: Bool?

    enum CodingKeys: String, CodingKey {
        case kind, id, enabled, endpoint
        case apiKeyConfigured = "api_key_configured"
    }
}

struct SharedCoreMaintenanceCommand: Decodable {
    let kind: String
    let channel: String?
}
