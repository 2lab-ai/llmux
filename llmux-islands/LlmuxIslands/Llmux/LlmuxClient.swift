import Foundation

/// Thin async client over the llmux daemon's HTTP control API. The app is a pure
/// consumer of this surface — it never reads `~/.config/llmux.json` or touches
/// provider credentials (`.prd/11-llmux-islands-spec.md` FR4). Defaults to the
/// loopback daemon (`http://127.0.0.1:3456`), which llmux exempts from the
/// `x-api-key` gate; an `apiKey` is only needed to reach a remote daemon.
struct LlmuxClient: Sendable {
    var baseURL: String
    var apiKey: String?

    init(host: String = "127.0.0.1", port: Int = 3456, apiKey: String? = nil) {
        self.baseURL = "http://\(host):\(port)"
        self.apiKey = apiKey
    }

    /// Build a client from the user's saved connection settings (Settings window).
    static func current() -> LlmuxClient {
        LlmuxClient(
            host: LlmuxSettings.host,
            port: LlmuxSettings.port,
            apiKey: LlmuxSettings.apiKey.isEmpty ? nil : LlmuxSettings.apiKey
        )
    }

    private func makeRequest(_ path: String, method: String = "GET", json: [String: Any]? = nil) -> URLRequest {
        var req = URLRequest(url: URL(string: baseURL + path)!)
        req.httpMethod = method
        req.timeoutInterval = 10
        if let apiKey, !apiKey.isEmpty {
            req.setValue(apiKey, forHTTPHeaderField: "x-api-key")
        }
        if let json {
            req.setValue("application/json", forHTTPHeaderField: "content-type")
            req.httpBody = try? JSONSerialization.data(withJSONObject: json)
        }
        return req
    }

    private func send(_ req: URLRequest) async throws -> Data {
        let (data, resp) = try await URLSession.shared.data(for: req)
        if let http = resp as? HTTPURLResponse, !(200..<300).contains(http.statusCode) {
            throw LlmuxError.http(http.statusCode, Self.errorMessage(from: data))
        }
        return data
    }

    /// `GET /llmux/status` — accounts + usage windows (FR1).
    func status() async throws -> LlmuxStatus {
        let data = try await send(makeRequest("/llmux/status"))
        return try JSONDecoder().decode(LlmuxStatus.self, from: data)
    }

    /// `POST /llmux/add-account` — add an Anthropic API-key account (FR2).
    func addApiKey(name: String?, apiKey: String) async throws {
        var body: [String: Any] = ["api_key": apiKey]
        if let name, !name.isEmpty { body["name"] = name }
        _ = try await send(makeRequest("/llmux/add-account", method: "POST", json: body))
    }

    /// `POST /llmux/remove-account` — remove an account by name (FR3).
    func remove(name: String) async throws {
        _ = try await send(makeRequest("/llmux/remove-account", method: "POST", json: ["name": name, "confirm": true]))
    }

    /// `POST /llmux/login/start` — begin a daemon-run OAuth login (FR4).
    func startLogin(provider: String) async throws -> LoginStartResponse {
        let data = try await send(makeRequest("/llmux/login/start", method: "POST", json: ["provider": provider]))
        return try JSONDecoder().decode(LoginStartResponse.self, from: data)
    }

    /// `GET /llmux/login/status?state=…` — poll an in-progress login (FR4).
    func loginStatus(state: String) async throws -> LoginStatusResponse {
        let encoded = state.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed) ?? state
        let data = try await send(makeRequest("/llmux/login/status?state=\(encoded)"))
        return try JSONDecoder().decode(LoginStatusResponse.self, from: data)
    }

    /// `POST /llmux/login/cancel` — abandon an in-progress login (FR4).
    func cancelLogin(state: String) async {
        _ = try? await send(makeRequest("/llmux/login/cancel", method: "POST", json: ["state": state]))
    }

    private static func errorMessage(from data: Data) -> String {
        if let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any] {
            if let err = obj["error"] as? [String: Any], let m = err["message"] as? String { return m }
            if let m = obj["error"] as? String { return m }
            if let m = obj["message"] as? String { return m }
        }
        return String(data: data, encoding: .utf8) ?? "request failed"
    }
}

enum LlmuxError: LocalizedError {
    case http(Int, String)

    var errorDescription: String? {
        switch self {
        case let .http(code, msg): return "HTTP \(code): \(msg)"
        }
    }
}
