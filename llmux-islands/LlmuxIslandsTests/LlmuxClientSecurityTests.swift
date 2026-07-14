import XCTest

final class LlmuxClientSecurityTests: XCTestCase {
    func testLoopbackDefaultsToHTTPIncludingStrictIPv4AndIPv6() throws {
        XCTAssertEqual(try LlmuxClient(host: "127.0.0.1").validatedEndpoint(), "http://127.0.0.1:3456")
        XCTAssertEqual(try LlmuxClient(host: "127.42.3.9").validatedEndpoint(), "http://127.42.3.9:3456")
        XCTAssertEqual(try LlmuxClient(host: "[::1]").validatedEndpoint(), "http://[::1]:3456")
        XCTAssertEqual(try LlmuxClient(host: "localhost").validatedEndpoint(), "http://localhost:3456")
    }

    func testRemoteDefaultsToHTTPSAndLoopbackLookalikeIsRemote() throws {
        XCTAssertEqual(
            try LlmuxClient(host: "llmux.example", apiKey: "configured").validatedEndpoint(),
            "https://llmux.example:3456"
        )
        XCTAssertEqual(
            try LlmuxClient(host: "127.evil.example", apiKey: "configured").validatedEndpoint(),
            "https://127.evil.example:3456"
        )
    }

    func testRemoteWithoutAPIKeyFailsClosedWithoutReflectingHost() {
        let client = LlmuxClient(host: "secret-shaped-host.example")
        XCTAssertThrowsError(try client.validatedEndpoint()) { error in
            XCTAssertEqual(error.localizedDescription, "Remote llmux endpoints require an API key.")
            XCTAssertFalse(error.localizedDescription.contains("secret-shaped-host"))
        }
    }

    func testRemoteConfigurationCanBePersistedUnauthenticatedAfterExplicitClear() throws {
        let client = LlmuxClient(host: "llmux.example")

        XCTAssertThrowsError(try client.validatedEndpoint())
        XCTAssertEqual(
            try client.validatedConnectionEndpoint(),
            "https://llmux.example:3456"
        )
        XCTAssertTrue(try client.isRemoteConnectionEndpoint())
    }

    func testApiKeyIntentDistinguishesKeepReplaceAndClear() {
        XCTAssertEqual(ConnectionApiKeyIntent.keep.resolvedKey(existing: "stored"), "stored")
        XCTAssertEqual(
            ConnectionApiKeyIntent.replace("  replacement  ").resolvedKey(existing: "stored"),
            "replacement"
        )
        XCTAssertEqual(ConnectionApiKeyIntent.clear.resolvedKey(existing: "stored"), "")
    }

    func testFreshLocalToRemoteBlankKeepCannotPersistUnauthenticatedConnection() throws {
        XCTAssertThrowsError(
            try ConnectionPersistencePlan.build(
                host: "llmux.example",
                port: 3456,
                apiKeyIntent: .keep,
                existingHost: "127.0.0.1",
                existingPort: 3456,
                existingKey: "",
                existingKeyWasExplicitlyCleared: false
            )
        ) { error in
            XCTAssertEqual(
                error.localizedDescription,
                "Remote llmux endpoints require an API key."
            )
        }
    }

    func testExplicitClearCanBeKeptOnlyForTheSameRemoteEndpoint() throws {
        let cleared = try ConnectionPersistencePlan.build(
            host: "llmux.example",
            port: 3456,
            apiKeyIntent: .clear,
            existingHost: "old.example",
            existingPort: 3456,
            existingKey: "stored",
            existingKeyWasExplicitlyCleared: false
        )
        XCTAssertEqual(cleared.apiKey, "")
        XCTAssertTrue(cleared.apiKeyWasExplicitlyCleared)

        let kept = try ConnectionPersistencePlan.build(
            host: cleared.host,
            port: cleared.port,
            apiKeyIntent: .keep,
            existingHost: cleared.host,
            existingPort: cleared.port,
            existingKey: cleared.apiKey,
            existingKeyWasExplicitlyCleared: cleared.apiKeyWasExplicitlyCleared
        )
        XCTAssertEqual(kept.apiKey, "")
        XCTAssertTrue(kept.apiKeyWasExplicitlyCleared)
        XCTAssertEqual(kept.endpoint, cleared.endpoint)

        XCTAssertThrowsError(
            try ConnectionPersistencePlan.build(
                host: "other.example",
                port: 3456,
                apiKeyIntent: .keep,
                existingHost: cleared.host,
                existingPort: cleared.port,
                existingKey: "",
                existingKeyWasExplicitlyCleared: true
            )
        )
    }

    func testControlKeyIsSentOnlyToRemoteDaemon() throws {
        let loopback = LlmuxClient(host: "127.0.0.1", apiKey: "must-not-leave")
        XCTAssertNil(try loopback.makeRequest("/llmux/status").value(forHTTPHeaderField: "x-api-key"))

        let remote = LlmuxClient(host: "llmux.example", apiKey: "configured")
        XCTAssertEqual(
            try remote.makeRequest("/llmux/status").value(forHTTPHeaderField: "x-api-key"),
            "configured"
        )
    }

    func testExplicitRemoteHTTPIsRejectedWithoutEchoingSecretShapedInput() {
        let client = LlmuxClient(host: "http://llmux.example")
        XCTAssertThrowsError(try client.validatedEndpoint()) { error in
            let message = error.localizedDescription
            XCTAssertEqual(
                message,
                "Remote llmux endpoints must use HTTPS. HTTP is allowed only for loopback."
            )
            XCTAssertFalse(message.contains("super-secret"))
            XCTAssertFalse(message.contains("llmux.example"))
        }
    }

    func testEndpointRejectsCredentialsAndURLTailBeforeSchemePolicy() {
        for invalid in [
            "https://user:super-secret@llmux.example",
            "https://llmux.example/private",
            "https://llmux.example?token=super-secret",
            "https://llmux.example#super-secret",
            "http://user:super-secret@llmux.example",
        ] {
            let client = LlmuxClient(host: invalid, apiKey: "configured")
            XCTAssertThrowsError(try client.validatedEndpoint(), invalid) { error in
                XCTAssertEqual(error.localizedDescription, "Invalid llmux endpoint.")
                XCTAssertFalse(error.localizedDescription.contains("super-secret"))
                XCTAssertFalse(error.localizedDescription.contains("llmux.example"))
            }
        }
    }

    func testEndpointRejectsOutOfRangePorts() {
        for invalidPort in [-1, 0, 65_536] {
            let client = LlmuxClient(host: "127.0.0.1", port: invalidPort)
            XCTAssertThrowsError(try client.validatedEndpoint(), String(invalidPort)) { error in
                XCTAssertEqual(error.localizedDescription, "Invalid llmux endpoint.")
            }
        }
    }

    func testInvalidEndpointErrorDoesNotReflectInput() {
        let client = LlmuxClient(host: "not a host?token=super-secret")
        XCTAssertThrowsError(try client.validatedEndpoint()) { error in
            XCTAssertEqual(error.localizedDescription, "Invalid llmux endpoint.")
            XCTAssertFalse(error.localizedDescription.contains("super-secret"))
        }
    }

    func testDaemonErrorBodyCannotReachLocalizedError() {
        let error = LlmuxError.http(500, "sk-secret-shaped-daemon-body")
        XCTAssertEqual(error.localizedDescription, "HTTP 500: request failed")
        XCTAssertFalse(error.localizedDescription.contains("sk-secret"))
    }

    func testLegacyStatusFallbackRequiresExplicitUnsupportedDashboardEndpoint() {
        for code in [404, 405, 501] {
            XCTAssertTrue(LlmuxError.http(code, "ignored").isUnsupportedDashboardEndpoint)
        }
        for code in [400, 401, 403, 408, 429, 500, 502, 503] {
            XCTAssertFalse(LlmuxError.http(code, "ignored").isUnsupportedDashboardEndpoint)
        }
        XCTAssertFalse(LlmuxError.invalidResponse.isUnsupportedDashboardEndpoint)
    }
}
