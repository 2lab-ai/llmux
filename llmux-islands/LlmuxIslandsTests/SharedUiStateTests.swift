import XCTest

final class SharedUiStateTests: XCTestCase {
    func testCoreConfigurationCarriesStartupPlatformPreferencesWithoutSecrets() throws {
        let configuration = SharedUiCoreConfiguration(
            endpointDisplay: "https://daemon.example.com:4567",
            remote: true,
            authenticated: true,
            apiKeyConfigured: true,
            selectedScreenID: "display-42",
            soundID: "glass",
            showFableWeekly: false,
            presentation: "regular"
        )
        let data = try JSONEncoder().encode(configuration)
        let root = try XCTUnwrap(try JSONSerialization.jsonObject(with: data) as? [String: Any])
        let platform = try XCTUnwrap(root["platform"] as? [String: Any])
        let connection = try XCTUnwrap(root["connection"] as? [String: Any])

        XCTAssertEqual(platform["selected_screen_id"] as? String, "display-42")
        XCTAssertEqual(platform["sound_id"] as? String, "glass")
        XCTAssertEqual(platform["show_fable_weekly"] as? Bool, false)
        XCTAssertEqual(connection["api_key_configured"] as? Bool, true)
        XCTAssertNil(connection["api_key"])
    }

    func testCanonicalStateCarriesOpaqueAccountsAnalyticsAndBothReceiptKinds() throws {
        let state = try JSONDecoder().decode(SharedUiState.self, from: Data(Self.fixture.utf8))

        XCTAssertEqual(state.schemaVersion, 1)
        XCTAssertEqual(state.revision, 9)
        XCTAssertTrue(state.window.open)
        XCTAssertEqual(state.window.contentHeight, 420)
        XCTAssertEqual(state.navigation, .statistics)
        XCTAssertTrue(state.connection.remote)
        XCTAssertEqual(state.usage.accounts.first?.id, "account-handle-7")
        XCTAssertEqual(state.usage.accounts.first?.displayName, "acc…ample.com")
        XCTAssertEqual(state.usage.currentByGroup["claude"], "account-handle-7")
        XCTAssertEqual(state.statistics.overview.requests, 12)
        XCTAssertEqual(state.statistics.activityReceipts.first?.receiptId, "request:1:POST:/v1/messages:200")
        XCTAssertEqual(state.statistics.activityReceipts.first?.path, "/v1/messages")
        XCTAssertEqual(state.verificationReceipts.first?.operation, "pause_account")
        XCTAssertEqual(state.verificationReceipts.first?.outcome, "succeeded")
        XCTAssertTrue(state.settings.emailAnonymous)
        XCTAssertTrue(state.settings.apiKeyConfigured)
        XCTAssertEqual(state.settings.soundID, "chime")
        XCTAssertEqual(state.settings.screens.first?.id, "screen-1")
        XCTAssertFalse(state.settings.capabilities.tray.available)
        XCTAssertEqual(state.settings.events.first?.id, "launch")
    }

    func testUnknownSchemaVersionIsRejected() {
        let incompatible = Self.fixture.replacingOccurrences(
            of: #""schema_version": 1"#,
            with: #""schema_version": 2"#
        )
        XCTAssertThrowsError(try JSONDecoder().decode(SharedUiState.self, from: Data(incompatible.utf8)))
    }

    private static let fixture = #"""
    {
      "schema_version": 1,
      "revision": 9,
      "lifecycle": "ready",
      "window": {
        "open": true,
        "open_reason": "click",
        "selected_screen_id": "screen-1",
        "presentation": "regular",
        "width": 600,
        "content_height": 420,
        "provider_in_flight": {"claude": 1}
      },
      "navigation": "statistics",
      "connection": {
        "endpoint_display": "https://llmux.example:3456",
        "remote": true,
        "authenticated": true,
        "daemon_version": "llmux 0.2.16",
        "last_success_ms": 2000,
        "retry_at_ms": null,
        "error": null
      },
      "usage": {
        "accounts": [{
          "id": "account-handle-7",
          "display_name": "acc…ample.com",
          "provider": "claude",
          "current": true,
          "paused": false,
          "healthy": true,
          "status": "ready",
          "blocked_reason": null,
          "in_flight": 1,
          "gauges": [{
            "kind": "five_hour",
            "available": true,
            "used_fraction": 0.25,
            "remaining_fraction": 0.75,
            "resets_at": 3600000,
            "reset_text": "1h 0m",
            "constraining": false
          }],
          "warning_level": "normal",
          "busy_action": null
        }],
        "current_by_group": {"claude": "account-handle-7"},
        "provider_in_flight": {"claude": 1},
        "login": {"phase": "idle"}
      },
      "statistics": {
        "overview": {
          "requests": 12, "ok": 11, "errors": 1,
          "tokens_in": 100, "tokens_out": 25,
          "rpm_5m": 1.5, "in_flight": 1, "cost_usd": 0.42
        },
        "models": [],
        "clients": [],
        "health": [],
        "heatmaps": [],
        "activity_receipts": [{
          "receipt_id": "request:1:POST:/v1/messages:200",
          "kind": "request",
          "occurred_at_ms": 1,
          "status": 200,
          "method": "POST",
          "path": "/v1/messages",
          "account_display": "acc…ample.com",
          "provider": "claude",
          "model": "claude-sonnet",
          "effort": null,
          "fast": false,
          "tokens": {"input": 2, "output": 1},
          "cache": null,
          "cost_usd": 0.01,
          "duration_ms": 10,
          "elapsed_ms": null,
          "message": null,
          "error": false
        }],
        "data_quality": {
          "model_usage": "canonical",
          "windowed": "best effort",
          "cost": "estimate",
          "cache": "unavailable is omitted"
        }
      },
      "settings": {
        "email_anonymous": true,
        "show_fable_weekly": true,
        "api_key_configured": true,
        "sound_id": "chime",
        "screens": [{"id": "screen-1", "label": "Built-in Display", "selected": true}],
        "sounds": [{"id": "chime", "label": "Chime", "selected": true}],
        "events": [{"id": "launch", "from": "202607140000", "to": "202607150000", "content": "Launch"}],
        "autostart": {"enabled": true, "available": true},
        "maintenance": {
          "channel": "stable",
          "version": "llmux 0.2.16",
          "islands_version": "0.2.3",
          "latest_version": null,
          "update_available": false,
          "install_owner": "homebrew",
          "license": "MIT",
          "source_url": "https://github.com/2lab-ai/llmux",
          "instructions": null
        },
        "capabilities": {
          "presentation": "regular",
          "remote": true,
          "layer_shell": {"available": false, "reason": "macOS regular window"},
          "tray": {"available": false, "reason": "not used by notch shell"},
          "notifications": {"available": true, "reason": "native notifications"}
        }
      },
      "notices": [],
      "verification_receipts": [{
        "id": "pause-1",
        "operation": "pause_account",
        "target_display": "acc…ample.com",
        "started_at_ms": 100,
        "finished_at_ms": 150,
        "outcome": "succeeded",
        "message": "account paused"
      }],
      "future_field": "ignored"
    }
    """#
}
