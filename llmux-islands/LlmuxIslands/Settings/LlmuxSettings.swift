import Foundation

/// User intent for the remote control credential. A blank editor value means
/// keep, never clear; deletion is an explicit, separate action.
enum ConnectionApiKeyIntent {
    case keep
    case replace(String)
    case clear

    func resolvedKey(existing: String) -> String {
        switch self {
        case .keep:
            return existing
        case let .replace(candidate):
            return candidate.trimmingCharacters(in: .whitespacesAndNewlines)
        case .clear:
            return ""
        }
    }

    /// Resolve the credential plus the durable proof that a missing remote key
    /// was an explicit user choice. A blank `.keep` may preserve an identical
    /// already-cleared endpoint, but it cannot turn a fresh/local setup into an
    /// unauthenticated remote configuration.
    func resolveForPersistence(
        existingKey: String,
        existingEndpoint: String?,
        existingKeyWasExplicitlyCleared: Bool,
        candidateEndpoint: String,
        candidateIsRemote: Bool
    ) throws -> ConnectionApiKeyResolution {
        let key = resolvedKey(existing: existingKey)
        let explicitlyCleared: Bool
        switch self {
        case .clear:
            explicitlyCleared = true
        case .keep:
            explicitlyCleared = key.isEmpty
                && existingKey.isEmpty
                && existingKeyWasExplicitlyCleared
                && existingEndpoint == candidateEndpoint
        case .replace:
            explicitlyCleared = false
        }

        if candidateIsRemote, key.isEmpty, !explicitlyCleared {
            throw LlmuxError.missingRemoteApiKey
        }
        return ConnectionApiKeyResolution(
            key: key,
            wasExplicitlyCleared: explicitlyCleared
        )
    }
}

struct ConnectionApiKeyResolution {
    let key: String
    let wasExplicitlyCleared: Bool
}

/// Fully validated, normalized connection transaction. Keeping planning in a
/// Foundation-only value lets the settings integration tests exercise the same
/// endpoint and credential policy that the live semantic executor persists.
struct ConnectionPersistencePlan {
    let host: String
    let port: Int
    let apiKey: String
    let apiKeyWasExplicitlyCleared: Bool
    let endpoint: String

    static func build(
        host rawHost: String,
        port: Int,
        apiKeyIntent: ConnectionApiKeyIntent,
        existingHost: String,
        existingPort: Int,
        existingKey: String,
        existingKeyWasExplicitlyCleared: Bool
    ) throws -> Self {
        let existingEndpoint = try? LlmuxClient(
            host: existingHost,
            port: existingPort,
            apiKey: existingKey.isEmpty ? nil : existingKey
        ).validatedConnectionEndpoint()
        let proposedKey = apiKeyIntent.resolvedKey(existing: existingKey)
        let candidate = LlmuxClient(
            host: rawHost,
            port: port,
            apiKey: proposedKey.isEmpty ? nil : proposedKey
        )
        let endpoint = try candidate.validatedConnectionEndpoint()
        let keyResolution = try apiKeyIntent.resolveForPersistence(
            existingKey: existingKey,
            existingEndpoint: existingEndpoint,
            existingKeyWasExplicitlyCleared: existingKeyWasExplicitlyCleared,
            candidateEndpoint: endpoint,
            candidateIsRemote: try candidate.isRemoteConnectionEndpoint()
        )
        guard let components = URLComponents(string: endpoint),
              let endpointHost = components.host
        else { throw LlmuxError.invalidEndpoint }

        let storedHost: String
        if rawHost.contains("://") {
            var hostOnly = URLComponents()
            hostOnly.scheme = components.scheme
            hostOnly.host = endpointHost
            guard let value = hostOnly.string else { throw LlmuxError.invalidEndpoint }
            storedHost = value
        } else if endpointHost.contains(":") {
            storedHost = "[\(endpointHost)]"
        } else {
            storedHost = endpointHost
        }
        return Self(
            host: storedHost,
            port: components.port ?? port,
            apiKey: keyResolution.key,
            apiKeyWasExplicitlyCleared: keyResolution.wasExplicitlyCleared,
            endpoint: endpoint
        )
    }
}

/// User-editable connection settings for the llmux daemon, backed by
/// UserDefaults. The Settings window writes these; `LlmuxClient.current()`
/// reads them. Defaults target the local loopback daemon.
enum LlmuxSettings {
    private static let defaults = UserDefaults.standard
    private static let apiKeyExplicitlyClearedKey = "llmux.apiKeyExplicitlyCleared"

    static var host: String {
        get { defaults.string(forKey: "llmux.host") ?? "127.0.0.1" }
        set { defaults.set(newValue, forKey: "llmux.host") }
    }

    static var port: Int {
        get {
            let v = defaults.integer(forKey: "llmux.port")
            return v == 0 ? 3456 : v
        }
        set { defaults.set(newValue, forKey: "llmux.port") }
    }

    static var apiKey: String {
        get { defaults.string(forKey: "llmux.apiKey") ?? "" }
        set {
            defaults.set(newValue, forKey: "llmux.apiKey")
            // Raw writes carry no explicit-clear intent. The connection
            // transaction restores this marker only after its semantic effect
            // and candidate endpoint have both been validated.
            defaults.set(false, forKey: apiKeyExplicitlyClearedKey)
        }
    }

    static var apiKeyWasExplicitlyCleared: Bool {
        get { defaults.bool(forKey: apiKeyExplicitlyClearedKey) }
        set { defaults.set(newValue, forKey: apiKeyExplicitlyClearedKey) }
    }
}
