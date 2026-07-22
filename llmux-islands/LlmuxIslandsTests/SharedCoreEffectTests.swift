import XCTest

final class SharedCoreEffectTests: XCTestCase {
    func testOpaqueHandleResolvesOnlyInsideTransientAccountEffect() throws {
        let data = Data(#"""
        [
          {
            "type":"run_operation",
            "operation_id":"pause-1",
            "request":{
              "kind":"pause_account",
              "account_id":"claude:raw-daemon-account@example.com",
              "paused":true
            }
          }
        ]
        """#.utf8)

        let effect = try XCTUnwrap(JSONDecoder().decode([SharedCoreEffect].self, from: data).first)
        XCTAssertEqual(effect.type, "run_operation")
        XCTAssertEqual(effect.operationID, "pause-1")
        XCTAssertEqual(effect.request?.kind, "pause_account")
        XCTAssertEqual(effect.request?.accountID, "claude:raw-daemon-account@example.com")
        XCTAssertEqual(effect.request?.paused, true)
    }

    func testAddEffectCarriesRequirementButNeverSecret() throws {
        let data = Data(#"""
        [
          {
            "type":"run_operation",
            "operation_id":"add-1",
            "request":{
              "kind":"add_account",
              "name":"work",
              "api_key_required":true
            }
          }
        ]
        """#.utf8)

        let effect = try XCTUnwrap(JSONDecoder().decode([SharedCoreEffect].self, from: data).first)
        XCTAssertEqual(effect.request?.kind, "add_account")
        XCTAssertEqual(effect.request?.apiKeyRequired, true)
        XCTAssertFalse(String(decoding: data, as: UTF8.self).contains("sk-secret"))
    }

    func testDedicatedPlatformEffectsDecodeTheirTypedPayloads() throws {
        let data = Data(#"""
        [
          {"type":"persist_settings","operation_id":"settings-1","change":{"kind":"connection_applied","endpoint":"https://llmux.example:3456","api_key_configured":true}},
          {"type":"run_maintenance","operation_id":"maintenance-1","command":{"kind":"change_channel","channel":"preview"}},
          {"type":"upsert_event","operation_id":"event-1","event":{"id":"notice","from":"202607140900","to":"202607141000","content":"maintenance"}}
        ]
        """#.utf8)

        let effects = try JSONDecoder().decode([SharedCoreEffect].self, from: data)
        XCTAssertEqual(effects[0].change?.kind, "connection_applied")
        XCTAssertEqual(effects[0].change?.apiKeyConfigured, true)
        XCTAssertEqual(effects[1].command?.channel, "preview")
        XCTAssertEqual(effects[2].event?.id, "notice")
    }
}
