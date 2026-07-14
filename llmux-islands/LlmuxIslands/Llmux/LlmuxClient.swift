import Foundation

/// Thin async client over the llmux daemon's HTTP control API. The app is a pure
/// consumer of this surface — it never reads `~/.config/llmux.json` or touches
/// provider credentials (`.prd/11-llmux-islands-spec.md` FR4). Defaults to the
/// loopback daemon (`http://127.0.0.1:3456`), which llmux exempts from the
/// `x-api-key` gate; an `apiKey` is only needed to reach a remote daemon.
struct LlmuxClient: Sendable {
    var baseURL: String
    var apiKey: String?
    private var configurationError: LlmuxError?

    private static let session: URLSession = {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.httpShouldSetCookies = false
        return URLSession(
            configuration: configuration,
            delegate: NoRedirectURLSessionDelegate.shared,
            delegateQueue: nil
        )
    }()

    init(host: String = "127.0.0.1", port: Int = 3456, apiKey: String? = nil) {
        let endpoint = Self.resolveEndpoint(host: host, port: port)
        self.baseURL = endpoint.url
        self.apiKey = apiKey
        if let error = endpoint.error {
            self.configurationError = error
        } else if let resolvedHost = URLComponents(string: endpoint.url)?.host,
                  !Self.isLoopback(resolvedHost),
                  apiKey?.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty != false {
            self.configurationError = .missingRemoteApiKey
        } else {
            self.configurationError = nil
        }
    }

    /// Build a client from the user's saved connection settings (Settings window).
    static func current() -> LlmuxClient {
        LlmuxClient(
            host: LlmuxSettings.host,
            port: LlmuxSettings.port,
            apiKey: LlmuxSettings.apiKey.isEmpty ? nil : LlmuxSettings.apiKey
        )
    }

    /// Sanitized endpoint passed to the semantic core. The API key is never
    /// embedded in this value or any UiState field.
    func validatedEndpoint() throws -> String {
        if let configurationError { throw configurationError }
        return baseURL
    }

    /// Validate and normalize connection shape without requiring that a remote
    /// credential is already present. This is not persistence authorization:
    /// `ConnectionPersistencePlan` separately admits a missing key only for an
    /// explicit clear or an identical previously-cleared remote endpoint.
    /// Request construction still fails closed through `validatedEndpoint()`.
    func validatedConnectionEndpoint() throws -> String {
        if let configurationError {
            guard case .missingRemoteApiKey = configurationError else {
                throw configurationError
            }
        }
        return baseURL
    }

    func isRemoteEndpoint() throws -> Bool {
        let endpoint = try validatedEndpoint()
        return try Self.isRemote(endpoint)
    }

    func isRemoteConnectionEndpoint() throws -> Bool {
        let endpoint = try validatedConnectionEndpoint()
        return try Self.isRemote(endpoint)
    }

    private static func isRemote(_ endpoint: String) throws -> Bool {
        guard let host = URLComponents(string: endpoint)?.host else {
            throw LlmuxError.invalidEndpoint
        }
        return !Self.isLoopback(host)
    }

    func makeRequest(_ path: String, method: String = "GET", json: [String: Any]? = nil) throws -> URLRequest {
        if let configurationError { throw configurationError }
        guard let url = URL(string: baseURL + path) else {
            throw LlmuxError.invalidEndpoint
        }
        var req = URLRequest(url: url)
        req.httpMethod = method
        req.timeoutInterval = 10
        let requestIsRemote = url.host.map { !Self.isLoopback($0) } ?? true
        if requestIsRemote, let apiKey, !apiKey.isEmpty {
            req.setValue(apiKey, forHTTPHeaderField: "x-api-key")
        }
        if let json {
            req.setValue("application/json", forHTTPHeaderField: "content-type")
            req.httpBody = try JSONSerialization.data(withJSONObject: json)
        }
        return req
    }

    private func send(_ req: URLRequest) async throws -> Data {
        let (data, resp) = try await Self.session.data(for: req)
        if let http = resp as? HTTPURLResponse, !(200..<300).contains(http.statusCode) {
            throw LlmuxError.http(http.statusCode, Self.errorMessage(from: data))
        }
        return data
    }

    /// `GET /llmux/status` — accounts + usage windows (FR1).
    func status() async throws -> LlmuxStatus {
        let data = try await statusData()
        return try JSONDecoder().decode(LlmuxStatus.self, from: data)
    }

    /// Raw legacy status bytes. They are normalized into a dashboard document
    /// and request-correlated through Rust; callers must not project them
    /// directly into SwiftUI state.
    func statusData() async throws -> Data {
        try await send(makeRequest("/llmux/status"))
    }

    /// Raw dashboard bytes are fed directly into the Rust semantic core. The
    /// Swift shell must not pre-decode or re-derive privacy-sensitive state.
    func dashboardData() async throws -> Data {
        try await send(makeRequest("/llmux/dashboard"))
    }

    /// `GET /llmux/dashboard` — the full dashboard document: the same
    /// `accounts[]` as `/llmux/status` plus totals / model / client / windowed
    /// / activity analytics (issue #62). Throws on transport, HTTP, or decode
    /// failure. Only an explicit unsupported-endpoint status is eligible for
    /// the request-correlated legacy adapter.
    func dashboard() async throws -> LlmuxDashboard {
        let data = try await dashboardData()
        return try JSONDecoder().decode(LlmuxDashboard.self, from: data)
    }

    /// `POST /llmux/add-account` — add an Anthropic API-key account (FR2).
    func addApiKey(name: String?, apiKey: String) async throws -> Bool {
        var body: [String: Any] = ["api_key": apiKey]
        if let name, !name.isEmpty { body["name"] = name }
        let data = try await send(makeRequest("/llmux/add-account", method: "POST", json: body))
        struct Ack: Decodable { let ok: Bool; let name: String; let added: Bool }
        let ack = try JSONDecoder().decode(Ack.self, from: data)
        guard ack.ok, !ack.name.isEmpty else { throw LlmuxError.invalidResponse }
        return ack.added
    }

    /// `POST /llmux/remove-account` — remove an account by name (FR3).
    func remove(name: String) async throws {
        let data = try await send(makeRequest("/llmux/remove-account", method: "POST", json: ["name": name, "confirm": true]))
        struct Ack: Decodable { let ok: Bool; let removed: Bool }
        let ack = try JSONDecoder().decode(Ack.self, from: data)
        guard ack.ok, ack.removed else { throw LlmuxError.invalidResponse }
    }

    /// `POST /llmux/pause-account` — pause/resume one account. The daemon
    /// persists the flag and the scheduler skips a paused account until it is
    /// resumed.
    func setPaused(name: String, paused: Bool) async throws {
        let data = try await send(makeRequest("/llmux/pause-account", method: "POST", json: ["account": name, "paused": paused]))
        struct Ack: Decodable { let ok: Bool; let paused: Bool }
        let ack = try JSONDecoder().decode(Ack.self, from: data)
        guard ack.ok, ack.paused == paused else { throw LlmuxError.invalidResponse }
    }

    /// `POST /llmux/settings` — flip the server-owned email-anonymous display
    /// setting. The daemon persists it (read-merge-write) and applies it live;
    /// returns the acknowledged effective value.
    func setEmailAnonymous(_ enabled: Bool) async throws -> Bool {
        let data = try await send(makeRequest("/llmux/settings", method: "POST", json: ["email_anonymous": enabled]))
        struct Ack: Decodable {
            let ok: Bool
            let emailAnonymous: Bool
            enum CodingKeys: String, CodingKey { case ok; case emailAnonymous = "email_anonymous" }
        }
        let ack = try JSONDecoder().decode(Ack.self, from: data)
        guard ack.ok else { throw LlmuxError.invalidResponse }
        return ack.emailAnonymous
    }

    /// `POST /llmux/events` with `{id, from, to, content}` — idempotent upsert
    /// of ONE event by id. 200 echoes `{"ok": true, "events": [<stored list>]}`;
    /// 400 on validation failure (non-empty id/content, parseable from/to,
    /// from < to). Returns nil only when the echo shape is unrecognized (the
    /// caller keeps its local list and refreshes via the dashboard).
    func upsertEvent(_ event: LlmuxEvent) async throws -> [LlmuxEvent]? {
        let data = try await send(makeRequest("/llmux/events", method: "POST", json: event.jsonObject))
        return LlmuxEventList.decode(data)
    }

    /// `POST /llmux/events` with `{"remove": "<id>"}` — idempotent remove (an
    /// absent id is still 200). Same echo shape as the upsert.
    func removeEvent(id: String) async throws -> [LlmuxEvent]? {
        let data = try await send(makeRequest("/llmux/events", method: "POST", json: ["remove": id]))
        return LlmuxEventList.decode(data)
    }

    /// `POST /llmux/login/start` — begin a daemon-run OAuth login (FR4).
    func startLogin(provider: String) async throws -> LoginStartResponse {
        let data = try await send(makeRequest("/llmux/login/start", method: "POST", json: ["provider": provider]))
        let response = try JSONDecoder().decode(LoginStartResponse.self, from: data)
        guard response.ok, !response.state.isEmpty, response.provider == provider else {
            throw LlmuxError.invalidResponse
        }
        return response
    }

    /// `GET /llmux/login/status?state=…` — poll an in-progress login (FR4).
    func loginStatus(state: String) async throws -> LoginStatusResponse {
        let encoded = state.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed) ?? state
        let data = try await send(makeRequest("/llmux/login/status?state=\(encoded)"))
        let response = try JSONDecoder().decode(LoginStatusResponse.self, from: data)
        if let verificationURI = response.verificationUri {
            guard let components = URLComponents(string: verificationURI),
                  components.user == nil,
                  components.password == nil,
                  let scheme = components.scheme?.lowercased(),
                  let host = components.host,
                  scheme == "https" || (scheme == "http" && Self.isLoopback(host))
            else { throw LlmuxError.invalidResponse }
        }
        return response
    }

    /// `POST /llmux/login/cancel` — abandon an in-progress login (FR4).
    func cancelLogin(state: String) async throws -> Bool {
        let data = try await send(makeRequest("/llmux/login/cancel", method: "POST", json: ["state": state]))
        struct Ack: Decodable { let ok: Bool; let cancelled: Bool }
        let ack = try JSONDecoder().decode(Ack.self, from: data)
        guard ack.ok else { throw LlmuxError.invalidResponse }
        return ack.cancelled
    }

    private static func errorMessage(from data: Data) -> String {
        // A daemon (or reverse proxy) controls this body and may echo request
        // fields. Never surface it into canonical receipts, offline labels, or
        // logs; the status code remains enough for diagnostics.
        _ = data
        return "request failed"
    }

    private static func resolveEndpoint(host rawHost: String, port: Int) -> (url: String, error: LlmuxError?) {
        let trimmed = rawHost.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, (1...65_535).contains(port) else {
            return ("", .invalidEndpoint)
        }

        let suppliedScheme = URLComponents(string: trimmed)?.scheme?.lowercased()
        let parseTarget = suppliedScheme == nil ? "//\(trimmed)" : trimmed
        var suppliedComponents = URLComponents(string: parseTarget)
        if suppliedScheme == nil,
           suppliedComponents?.host == nil,
           trimmed.contains(":"),
           !trimmed.hasPrefix("[") {
            // A bare IPv6 literal needs brackets only for URL parsing. Keep the
            // persisted host representation separate from this normalization.
            suppliedComponents = URLComponents(string: "//[\(trimmed)]")
        }

        guard let suppliedComponents,
              suppliedComponents.url != nil,
              let host = suppliedComponents.host,
              !host.isEmpty,
              suppliedComponents.user == nil,
              suppliedComponents.password == nil,
              suppliedComponents.query == nil,
              suppliedComponents.fragment == nil,
              suppliedComponents.path.isEmpty || suppliedComponents.path == "/",
              suppliedComponents.port != 0
        else {
            return ("", .invalidEndpoint)
        }

        let loopback = isLoopback(host)
        let scheme = suppliedScheme ?? (loopback ? "http" : "https")

        guard scheme == "http" || scheme == "https", !host.isEmpty else {
            return ("", .invalidEndpoint)
        }
        if scheme == "http", !loopback {
            return ("", .insecureRemoteEndpoint)
        }

        var components = URLComponents()
        components.scheme = scheme
        components.host = host
        components.port = port
        guard let url = components.url?.absoluteString else {
            return ("", .invalidEndpoint)
        }
        return (url.hasSuffix("/") ? String(url.dropLast()) : url, nil)
    }

    private static func isLoopback(_ host: String) -> Bool {
        let normalized = host.trimmingCharacters(in: CharacterSet(charactersIn: "[]")).lowercased()
        if normalized == "localhost" || normalized == "::1" { return true }
        let octets = normalized.split(separator: ".", omittingEmptySubsequences: false)
        guard octets.count == 4,
              let first = UInt8(octets[0]), first == 127,
              octets.dropFirst().allSatisfy({ UInt8($0) != nil })
        else { return false }
        return true
    }
}

enum LlmuxError: LocalizedError {
    case http(Int, String)
    case invalidEndpoint
    case insecureRemoteEndpoint
    case missingRemoteApiKey
    case invalidResponse

    var errorDescription: String? {
        switch self {
        case let .http(code, _): return "HTTP \(code): request failed"
        case .invalidEndpoint: return "Invalid llmux endpoint."
        case .insecureRemoteEndpoint:
            return "Remote llmux endpoints must use HTTPS. HTTP is allowed only for loopback."
        case .missingRemoteApiKey:
            return "Remote llmux endpoints require an API key."
        case .invalidResponse: return "The llmux daemon returned an invalid response."
        }
    }

    /// Compatibility is intentionally narrow: transport failures, auth
    /// failures, server errors, and malformed dashboard JSON must remain real
    /// failures instead of being hidden by a second uncorrelated read path.
    var isUnsupportedDashboardEndpoint: Bool {
        guard case let .http(code, _) = self else { return false }
        return code == 404 || code == 405 || code == 501
    }
}

private final class NoRedirectURLSessionDelegate: NSObject, URLSessionTaskDelegate {
    static let shared = NoRedirectURLSessionDelegate()

    func urlSession(
        _ session: URLSession,
        task: URLSessionTask,
        willPerformHTTPRedirection response: HTTPURLResponse,
        newRequest request: URLRequest,
        completionHandler: @escaping (URLRequest?) -> Void
    ) {
        completionHandler(nil)
    }
}
