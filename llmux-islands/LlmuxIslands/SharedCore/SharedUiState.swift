import Foundation

/// Secret-free configuration encoded across the Swift/Rust bridge. Platform
/// preferences are included at startup so a newly created runtime publishes
/// the same canonical local state before any setting effect is dispatched.
struct SharedUiCoreConfiguration: Encodable {
    let endpointDisplay: String
    let remote: Bool
    let authenticated: Bool
    let apiKeyConfigured: Bool
    let selectedScreenID: String
    let soundID: String
    let showFableWeekly: Bool
    let presentation: String

    private enum CodingKeys: String, CodingKey {
        case connection, platform
    }

    private enum ConnectionKeys: String, CodingKey {
        case remote, authenticated
        case endpointDisplay = "endpoint_display"
        case apiKeyConfigured = "api_key_configured"
    }

    private enum PlatformKeys: String, CodingKey {
        case presentation
        case selectedScreenID = "selected_screen_id"
        case soundID = "sound_id"
        case showFableWeekly = "show_fable_weekly"
    }

    func encode(to encoder: Encoder) throws {
        var root = encoder.container(keyedBy: CodingKeys.self)
        var connection = root.nestedContainer(keyedBy: ConnectionKeys.self, forKey: .connection)
        try connection.encode(endpointDisplay, forKey: .endpointDisplay)
        try connection.encode(remote, forKey: .remote)
        try connection.encode(authenticated, forKey: .authenticated)
        try connection.encode(apiKeyConfigured, forKey: .apiKeyConfigured)
        var platform = root.nestedContainer(keyedBy: PlatformKeys.self, forKey: .platform)
        try platform.encode(selectedScreenID, forKey: .selectedScreenID)
        try platform.encode(soundID, forKey: .soundID)
        try platform.encode(showFableWeekly, forKey: .showFableWeekly)
        try platform.encode(presentation, forKey: .presentation)
    }
}

/// Swift mirror of `llmux-islands-core`'s versioned semantic UI contract.
///
/// This is deliberately a display contract, not a second derivation layer:
/// account aliases, privacy filtering, quota gauges, analytics and receipts
/// have already been produced by Rust. AppKit/SwiftUI only project those
/// canonical values into the existing native macOS views.
struct SharedUiState: Decodable, Equatable {
    let schemaVersion: UInt32
    let revision: UInt64
    let lifecycle: String
    let window: SharedWindowState
    let navigation: SharedNavigation
    let connection: SharedConnectionState
    let usage: SharedUsageState
    let statistics: SharedStatisticsState
    let settings: SharedSettingsState
    let operation: SharedOperationState?
    let notices: [SharedNotice]
    let verificationReceipts: [SharedVerificationReceipt]

    enum CodingKeys: String, CodingKey {
        case revision, lifecycle, window, navigation, connection, usage, statistics, settings, operation, notices
        case schemaVersion = "schema_version"
        case verificationReceipts = "verification_receipts"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        schemaVersion = try container.decode(UInt32.self, forKey: .schemaVersion)
        guard schemaVersion == 1 else {
            throw DecodingError.dataCorruptedError(
                forKey: .schemaVersion,
                in: container,
                debugDescription: "unsupported shared UI schema version"
            )
        }
        revision = try container.decode(UInt64.self, forKey: .revision)
        lifecycle = try container.decode(String.self, forKey: .lifecycle)
        window = try container.decode(SharedWindowState.self, forKey: .window)
        navigation = try container.decode(SharedNavigation.self, forKey: .navigation)
        connection = try container.decode(SharedConnectionState.self, forKey: .connection)
        usage = try container.decode(SharedUsageState.self, forKey: .usage)
        statistics = try container.decode(SharedStatisticsState.self, forKey: .statistics)
        settings = try container.decode(SharedSettingsState.self, forKey: .settings)
        operation = try container.decodeIfPresent(SharedOperationState.self, forKey: .operation)
        notices = try container.decode([SharedNotice].self, forKey: .notices)
        verificationReceipts = try container.decode(
            [SharedVerificationReceipt].self,
            forKey: .verificationReceipts
        )
    }

}

struct SharedWindowState: Decodable, Equatable {
    let open: Bool
    let openReason: String
    let selectedScreenID: String
    let presentation: String
    let width, contentHeight: UInt32
    let providerInFlight: [String: UInt32]

    enum CodingKeys: String, CodingKey {
        case open, presentation, width
        case openReason = "open_reason"
        case selectedScreenID = "selected_screen_id"
        case contentHeight = "content_height"
        case providerInFlight = "provider_in_flight"
    }
}

enum SharedNavigation: String, Decodable, Equatable {
    case usage, statistics, menu
}

struct SharedConnectionState: Decodable, Equatable {
    let endpointDisplay: String
    let remote, authenticated: Bool
    let daemonVersion: String?
    let lastSuccessMs: UInt64?
    let retryAtMs: UInt64?
    let error: String?

    enum CodingKeys: String, CodingKey {
        case remote, authenticated, error
        case endpointDisplay = "endpoint_display"
        case daemonVersion = "daemon_version"
        case lastSuccessMs = "last_success_ms"
        case retryAtMs = "retry_at_ms"
    }
}

struct SharedUsageState: Decodable, Equatable {
    let accounts: [SharedAccountTile]
    let currentByGroup: [String: String]
    let providerInFlight: [String: UInt32]
    let login: SharedLoginState

    enum CodingKeys: String, CodingKey {
        case accounts, login
        case currentByGroup = "current_by_group"
        case providerInFlight = "provider_in_flight"
    }
}

struct SharedAccountTile: Decodable, Equatable {
    let id: String
    let displayName: String
    let provider: SharedProvider
    let current: Bool
    let paused: Bool
    let healthy: Bool
    let status: String
    let blockedReason: String?
    let inFlight: UInt32
    let tokenExpiry: SharedTokenExpiry?
    let gauges: [SharedGauge]
    let warningLevel: String
    let busyAction: String?

    enum CodingKeys: String, CodingKey {
        case id, provider, current, paused, healthy, status, gauges
        case displayName = "display_name"
        case blockedReason = "blocked_reason"
        case inFlight = "in_flight"
        case tokenExpiry = "token_expiry"
        case warningLevel = "warning_level"
        case busyAction = "busy_action"
    }

    func gauge(_ kind: SharedGauge.Kind) -> SharedGauge? {
        gauges.first { $0.kind == kind && $0.available }
    }

}

enum SharedProvider: String, Decodable, Equatable {
    case claude, codex, grok, api, unknown
}

struct SharedTokenExpiry: Decodable, Equatable {
    let state: String
    let expiresAtMs: UInt64
    let countdownText: String

    enum CodingKeys: String, CodingKey {
        case state
        case expiresAtMs = "expires_at_ms"
        case countdownText = "countdown_text"
    }
}

struct SharedGauge: Decodable, Equatable {
    enum Kind: String, Decodable, Equatable {
        case fiveHour = "five_hour"
        case sevenDay = "seven_day"
        case fableWeekly = "fable_weekly"
    }

    let kind: Kind
    let available: Bool
    let usedFraction: Double
    let remainingFraction: Double
    let resetsAt: UInt64?
    let resetText: String?
    let constraining: Bool

    enum CodingKeys: String, CodingKey {
        case kind, available, constraining
        case usedFraction = "used_fraction"
        case remainingFraction = "remaining_fraction"
        case resetsAt = "resets_at"
        case resetText = "reset_text"
    }

}

struct SharedLoginState: Decodable, Equatable {
    let phase: String
    let provider: String?
    let state: String?
    let verificationUri: String?
    let userCode: String?
    let message: String?

    enum CodingKeys: String, CodingKey {
        case phase, provider, state, message
        case verificationUri = "verification_uri"
        case userCode = "user_code"
    }
}

struct SharedStatisticsState: Decodable, Equatable {
    let overview: SharedOverview
    let models: [SharedModelStatistics]
    let clients: [SharedClientStatistics]
    let health: [SharedHealthStatistics]
    let heatmaps: [SharedHeatmapStatistics]
    let activityReceipts: [SharedActivityReceipt]
    let dataQuality: SharedDataQuality

    enum CodingKeys: String, CodingKey {
        case overview, models, clients, health, heatmaps
        case activityReceipts = "activity_receipts"
        case dataQuality = "data_quality"
    }
}

struct SharedOverview: Decodable, Equatable {
    let requests, ok, errors, tokensIn, tokensOut: UInt64
    let rpm5m: Double
    let inFlight: Int
    let costUsd: Double

    enum CodingKeys: String, CodingKey {
        case requests, ok, errors
        case tokensIn = "tokens_in"
        case tokensOut = "tokens_out"
        case rpm5m = "rpm_5m"
        case inFlight = "in_flight"
        case costUsd = "cost_usd"
    }

}

struct SharedModelStatistics: Decodable, Equatable {
    let group, model: String
    let requests, ok, errors, tokensIn, tokensOut: UInt64
    let cacheRead, cacheCreation: UInt64?
    let lastUsedMs: UInt64
    let inFlight: Int
    let accounts: [SharedModelAccountStatistics]
    let efforts, endpoints: [SharedStatisticsCount]
    let costUsd: Double

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

}

struct SharedStatisticsCount: Decodable, Equatable {
    let label: String
    let requests: UInt64
}

struct SharedModelAccountStatistics: Decodable, Equatable {
    let displayName: String
    let requests, ok, errors, tokensIn, tokensOut: UInt64

    enum CodingKeys: String, CodingKey {
        case requests, ok, errors
        case displayName = "display_name"
        case tokensIn = "tokens_in"
        case tokensOut = "tokens_out"
    }

}

struct SharedClientStatistics: Decodable, Equatable {
    let client: String
    let requests, ok, errors, tokensIn, tokensOut: UInt64
    let costUsd: Double
    let lastSeenMs: UInt64

    enum CodingKeys: String, CodingKey {
        case client, requests, ok, errors
        case tokensIn = "tokens_in"
        case tokensOut = "tokens_out"
        case costUsd = "cost_usd"
        case lastSeenMs = "last_seen_ms"
    }

}

struct SharedHealthStatistics: Decodable, Equatable {
    let id, displayName, kind, status: String
    let healthy, paused: Bool
    let blockedReason: String?
    let cooldownUntilMs: UInt64?
    let cooldownSource: String?
    let inFlight: UInt32
    let tokenExpiresAtMs, lastRefreshMs: UInt64?

    enum CodingKeys: String, CodingKey {
        case id, kind, healthy, paused, status
        case displayName = "display_name"
        case blockedReason = "blocked_reason"
        case cooldownUntilMs = "cooldown_until_ms"
        case cooldownSource = "cooldown_source"
        case inFlight = "in_flight"
        case tokenExpiresAtMs = "token_expires_at_ms"
        case lastRefreshMs = "last_refresh_ms"
    }
}

struct SharedHeatmapStatistics: Decodable, Equatable {
    let window: String
    let windowSecs: UInt64
    let cells: [SharedHeatmapCell]

    enum CodingKeys: String, CodingKey {
        case window, cells
        case windowSecs = "window_secs"
    }

}

struct SharedHeatmapCell: Decodable, Equatable {
    let group, model, accountDisplay: String
    let requests, ok, errors, tokensIn, tokensOut, cacheRead, cacheCreation, tokens: UInt64

    enum CodingKeys: String, CodingKey {
        case group, model, requests, ok, errors, tokens
        case accountDisplay = "account_display"
        case tokensIn = "tokens_in"
        case tokensOut = "tokens_out"
        case cacheRead = "cache_read"
        case cacheCreation = "cache_creation"
    }

}

struct SharedActivityReceipt: Decodable, Equatable, Identifiable {
    let receiptId, kind: String
    let occurredAtMs: UInt64
    let status: Int?
    let method, path, accountDisplay: String?
    let provider: SharedProvider?
    let model, effort: String?
    let fast: Bool
    let tokens: SharedReceiptTokens?
    let cache: SharedReceiptCache?
    let costUsd: Double?
    let durationMs, elapsedMs: UInt64?
    let message: String?
    let error: Bool
    // Activity client-name (additive — absent in older core states → nil).
    let tenant: String?
    let clientName: String?

    var id: String { receiptId }

    enum CodingKeys: String, CodingKey {
        case kind, status, method, path, provider, model, effort, fast, tokens, cache, message, error,
             tenant
        case receiptId = "receipt_id"
        case occurredAtMs = "occurred_at_ms"
        case accountDisplay = "account_display"
        case costUsd = "cost_usd"
        case durationMs = "duration_ms"
        case elapsedMs = "elapsed_ms"
        case clientName = "client_name"
    }

}

struct SharedReceiptTokens: Decodable, Equatable { let input, output: UInt64 }
struct SharedReceiptCache: Decodable, Equatable { let read, creation: UInt64? }

struct SharedDataQuality: Decodable, Equatable {
    let modelUsage, windowed, cost, cache: String

    enum CodingKeys: String, CodingKey {
        case windowed, cost, cache
        case modelUsage = "model_usage"
    }

}

struct SharedSettingsState: Decodable, Equatable {
    let emailAnonymous, showFableWeekly, apiKeyConfigured: Bool
    let soundID: String?
    let screens: [SharedScreenOption]
    let sounds: [SharedSoundOption]
    let events: [SharedEvent]
    let autostart: SharedAutostartState
    let maintenance: SharedMaintenanceState
    let capabilities: SharedCapabilitiesState

    enum CodingKeys: String, CodingKey {
        case screens, sounds, events, autostart, maintenance, capabilities
        case emailAnonymous = "email_anonymous"
        case showFableWeekly = "show_fable_weekly"
        case apiKeyConfigured = "api_key_configured"
        case soundID = "sound_id"
    }
}

struct SharedScreenOption: Decodable, Equatable, Identifiable {
    let id, label: String
    let selected: Bool
}

struct SharedSoundOption: Decodable, Equatable, Identifiable {
    let id, label: String
    let selected: Bool?
}

struct SharedAutostartState: Decodable, Equatable {
    let enabled: Bool?
    let available: Bool?
}

struct SharedMaintenanceState: Decodable, Equatable {
    let channel, version, latestVersion, installOwner, instructions: String?
    let islandsVersion, license, sourceURL: String
    let updateAvailable: Bool?

    enum CodingKeys: String, CodingKey {
        case channel, version, license, instructions
        case latestVersion = "latest_version"
        case installOwner = "install_owner"
        case islandsVersion = "islands_version"
        case sourceURL = "source_url"
        case updateAvailable = "update_available"
    }
}

struct SharedCapabilitiesState: Decodable, Equatable {
    let presentation: String?
    let remote: Bool?
    let layerShell, tray, notifications: SharedCapability

    enum CodingKeys: String, CodingKey {
        case presentation, remote, tray, notifications
        case layerShell = "layer_shell"
    }
}

struct SharedCapability: Decodable, Equatable {
    let available: Bool
    let reason: String
}

struct SharedEvent: Decodable, Equatable, Identifiable {
    let id, from, to, content: String
}

struct SharedNotice: Decodable, Equatable, Identifiable {
    let id, level, message: String
}

struct SharedOperationState: Decodable, Equatable, Identifiable {
    let id, kind: String
    let targetDisplay: String?
    let startedAtMs: UInt64

    enum CodingKeys: String, CodingKey {
        case id, kind
        case targetDisplay = "target_display"
        case startedAtMs = "started_at_ms"
    }
}

struct SharedVerificationReceipt: Decodable, Equatable, Identifiable {
    let id, operation: String
    let targetDisplay: String?
    let startedAtMs, finishedAtMs: UInt64
    let outcome, message: String

    enum CodingKeys: String, CodingKey {
        case id, operation, outcome, message
        case targetDisplay = "target_display"
        case startedAtMs = "started_at_ms"
        case finishedAtMs = "finished_at_ms"
    }
}
