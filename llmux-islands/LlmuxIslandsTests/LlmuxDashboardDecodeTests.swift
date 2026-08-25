import XCTest

/// Decode tests for the `GET /llmux/dashboard` contract (issue #62 S3).
///
/// Three families, mirroring the acceptance list:
/// (a) a full current-daemon document decodes and carries the values through,
/// (b) an old-daemon-shaped document (every additive field stripped) still
///     decodes — missing additive fields must NEVER fail the parse,
/// (c) broken payloads throw and remain protocol failures rather than
///     activating legacy compatibility.
final class LlmuxDashboardDecodeTests: XCTestCase {
    private func decode(_ json: String) throws -> LlmuxDashboard {
        try JSONDecoder().decode(LlmuxDashboard.self, from: Data(json.utf8))
    }

    private func decodeWire(_ json: String) throws -> LlmuxDashboardWireDocument {
        try JSONDecoder().decode(LlmuxDashboardWireDocument.self, from: Data(json.utf8))
    }

    // MARK: - (a) full document

    func testFullFixtureDecodes() throws {
        let wire = try decodeWire(DashboardFixtures.full)
        let dash = wire.dashboard

        XCTAssertEqual(dash.version, "llmux 0.2.14 (preview 2026-07-04-0558-4cc97acd45dc)")
        XCTAssertEqual(dash.port, 3456)
        XCTAssertEqual(dash.current, "claude:user1@example.com")
        XCTAssertEqual(dash.currentByGroup["claude"], "claude:user1@example.com")
        XCTAssertEqual(dash.currentByGroup["codex"], "codex:user1@example.com")
        XCTAssertEqual(dash.emailAnonymous, false)
        XCTAssertTrue(wire.hasEmailAnonymousField)
        XCTAssertEqual(dash.showFableWeekly, true)
        // `data_quality` ships in a later slice — absent today, decodes nil.
        XCTAssertNil(dash.dataQuality)
    }

    func testFullFixtureAccounts() throws {
        let dash = try decode(DashboardFixtures.full)
        XCTAssertEqual(dash.accounts.count, 3)

        let first = try XCTUnwrap(dash.accounts.first)
        XCTAssertEqual(first.name, "claude:user1@example.com")
        XCTAssertEqual(first.type, "oauth")
        XCTAssertEqual(first.status, "active")
        XCTAssertEqual(first.inFlight, 1)
        XCTAssertEqual(first.tokenExpiresAtMs, 1_783_170_183_305)
        XCTAssertEqual(try XCTUnwrap(first.fiveHour).utilization, 0.06, accuracy: 1e-9)
        XCTAssertEqual(try XCTUnwrap(first.fiveHour).source, "headers")

        // Fable weekly rides the same wire object as /llmux/status.
        let fable = try XCTUnwrap(first.fableWeekly)
        XCTAssertEqual(fable.utilization, 0.91, accuracy: 1e-9)
        XCTAssertEqual(fable.severity, "critical")
        XCTAssertEqual(fable.isActive, true)
        XCTAssertEqual(fable.constraining, false)

        // The codex accounts classify as codex via `type` alone — the
        // dashboard doc has no per-account `group` key.
        XCTAssertEqual(dash.accounts[1].type, "codex")
        XCTAssertEqual(dash.accounts[2].blocked, "7d 100.0% > 99%")
    }

    func testFullFixtureAnalytics() throws {
        let dash = try decode(DashboardFixtures.full)

        XCTAssertEqual(dash.totals.requests, 36633)
        XCTAssertEqual(dash.totals.tokensIn, 79_943_758)
        XCTAssertEqual(dash.totals.tokensOut, 29_670_001)
        XCTAssertEqual(dash.totals.totalTokens, 109_613_759)
        XCTAssertEqual(try XCTUnwrap(dash.totals.costUsd), 8689.70841815, accuracy: 1e-6)

        XCTAssertEqual(dash.modelUsage.count, 2)
        let opus = try XCTUnwrap(dash.modelUsage.first)
        XCTAssertEqual(opus.group, "claude")
        XCTAssertEqual(opus.model, "claude-opus-4-8")
        XCTAssertEqual(opus.requests, 30226)
        XCTAssertEqual(opus.cacheRead, 4_889_342_877)
        XCTAssertEqual(opus.inFlight, 1)
        XCTAssertFalse(opus.accounts.isEmpty)
        XCTAssertFalse(opus.efforts.isEmpty)
        XCTAssertFalse(opus.endpoints.isEmpty)
        // Per-row cost ships in slice S1 — the 0.2.14 daemon omits it.
        XCTAssertNil(opus.costUsd)

        XCTAssertEqual(dash.clientUsage.count, 3)
        XCTAssertEqual(dash.clientUsage[0].client, "unknown")
        XCTAssertNil(dash.clientUsage[0].costUsd)
        XCTAssertNil(dash.clientUsage[0].lastSeenMs)

        XCTAssertEqual(dash.windowed.map(\.window), ["24h", "72h"])
        XCTAssertEqual(dash.windowed[0].windowSecs, 86400)
        XCTAssertEqual(dash.windowed[0].cells.count, 2)
        let cell = dash.windowed[0].cells[0]
        XCTAssertEqual(cell.tokens, cell.tokensIn + cell.tokensOut + cell.cacheRead + cell.cacheCreation)
    }

    func testFullFixtureActivityIncludingNoteRows() throws {
        let dash = try decode(DashboardFixtures.full)

        XCTAssertEqual(dash.activity.inFlight.count, 1)
        XCTAssertEqual(dash.activity.inFlight[0].group, "claude")
        XCTAssertEqual(dash.activity.inFlight[0].model, "claude-opus-4-8")

        // `completed` is a kind-tagged enum on the wire — request AND note
        // rows must both decode (a note row would otherwise sink the doc).
        XCTAssertEqual(dash.activity.completed.count, 3)
        let requests = dash.activity.completed.filter(\.isRequest)
        XCTAssertEqual(requests.count, 2)
        XCTAssertEqual(requests[0].status, 200)
        XCTAssertEqual(try XCTUnwrap(requests[0].tokens).input, 106)
        XCTAssertEqual(try XCTUnwrap(requests[0].costUsd), 0.025631, accuracy: 1e-9)
        let note = try XCTUnwrap(dash.activity.completed.first(where: \.isNote))
        XCTAssertEqual(note.error, false)
        XCTAssertNotNil(note.text)
        XCTAssertNil(note.status)

        // The fixture predates the tenant/client-name fields (activity
        // client-name) — absent keys decode nil, never a parse error.
        XCTAssertNil(requests[0].tenant)
        XCTAssertNil(requests[0].clientName)
    }

    func testCompletedRowTenantAndClientNameDecode() throws {
        // A row as a current daemon writes it: tenant id + resolved name.
        let json = """
        {"kind": "request", "at_ms": 1783146111455, "method": "POST",
         "path": "/v1/messages", "account": "claude:user1@example.com",
         "status": 200, "duration_ms": 1463,
         "tenant": "k-abc123", "client_name": "Z (U09F1M5MML1)"}
        """
        let row = try JSONDecoder().decode(LlmuxDashboardCompleted.self, from: Data(json.utf8))
        XCTAssertEqual(row.tenant, "k-abc123")
        XCTAssertEqual(row.clientName, "Z (U09F1M5MML1)")
        XCTAssertEqual(ClientNameLabel.short(try XCTUnwrap(row.clientName)), "Z")
    }

    // MARK: - (b) old-daemon-shaped document

    /// Strip every additive-since-launch field from the full fixture, the way
    /// an older daemon's serializer simply never wrote them.
    private func oldDaemonJSON() throws -> String {
        var doc = try XCTUnwrap(
            try JSONSerialization.jsonObject(with: Data(DashboardFixtures.full.utf8)) as? [String: Any]
        )
        for key in ["current_by_group", "model_usage", "client_usage", "windowed",
                    "email_anonymous", "show_fable_weekly", "codex", "data_quality"] {
            doc.removeValue(forKey: key)
        }
        var totals = try XCTUnwrap(doc["totals"] as? [String: Any])
        totals.removeValue(forKey: "cost_usd")
        doc["totals"] = totals
        doc["accounts"] = try XCTUnwrap(doc["accounts"] as? [[String: Any]]).map { account in
            var account = account
            account.removeValue(forKey: "fable_weekly")
            account.removeValue(forKey: "scoped_limits")
            return account
        }
        var activity = try XCTUnwrap(doc["activity"] as? [String: Any])
        activity["in_flight"] = try XCTUnwrap(activity["in_flight"] as? [[String: Any]]).map { row in
            var row = row
            row.removeValue(forKey: "group")
            row.removeValue(forKey: "model")
            return row
        }
        activity["completed"] = try XCTUnwrap(activity["completed"] as? [[String: Any]]).map { row in
            var row = row
            for key in ["cost_usd", "group", "model", "effort"] { row.removeValue(forKey: key) }
            return row
        }
        doc["activity"] = activity
        let data = try JSONSerialization.data(withJSONObject: doc)
        return try XCTUnwrap(String(data: data, encoding: .utf8))
    }

    func testOldDaemonDocumentStillDecodes() throws {
        let wire = try decodeWire(try oldDaemonJSON())
        let dash = wire.dashboard

        // Missing additive fields degrade to defaults — never a parse error.
        XCTAssertTrue(dash.currentByGroup.isEmpty)
        XCTAssertTrue(dash.modelUsage.isEmpty)
        XCTAssertTrue(dash.clientUsage.isEmpty)
        XCTAssertTrue(dash.windowed.isEmpty)
        XCTAssertNil(dash.emailAnonymous)
        XCTAssertFalse(wire.hasEmailAnonymousField)
        XCTAssertNil(dash.showFableWeekly)
        XCTAssertNil(dash.dataQuality)
        XCTAssertNil(dash.totals.costUsd)
        XCTAssertEqual(dash.accounts.count, 3)
        XCTAssertNil(dash.accounts[0].fableWeekly)
        XCTAssertNil(dash.activity.inFlight[0].group)
        let request = try XCTUnwrap(dash.activity.completed.first(where: \.isRequest))
        XCTAssertNil(request.costUsd)
        XCTAssertEqual(request.status, 200)
    }

    func testExplicitNullRemainsDistinctFromAnOmittedEmailSetting() throws {
        var doc = try XCTUnwrap(
            try JSONSerialization.jsonObject(with: Data(DashboardFixtures.full.utf8)) as? [String: Any]
        )
        doc["email_anonymous"] = NSNull()
        let data = try JSONSerialization.data(withJSONObject: doc)
        let wire = try JSONDecoder().decode(LlmuxDashboardWireDocument.self, from: data)

        XCTAssertNil(wire.dashboard.emailAnonymous)
        XCTAssertTrue(wire.hasEmailAnonymousField)
    }

    func testOldDaemonNormalizationInjectsLocalEmailValueWithoutTransferringOwnership() throws {
        let original = Data(try oldDaemonJSON().utf8)
        let normalized = try LlmuxDashboardWireNormalizer.normalize(
            original,
            localEmailAnonymous: true
        )
        let object = try XCTUnwrap(
            try JSONSerialization.jsonObject(with: normalized.dashboardJSON) as? [String: Any]
        )

        XCTAssertEqual(object["email_anonymous"] as? Bool, true)
        XCTAssertFalse(normalized.serverOwnsEmailSetting)
        XCTAssertNotEqual(normalized.dashboardJSON, original)
    }

    func testDaemonEmailFieldRemainsByteExactAndServerOwned() throws {
        let original = Data(DashboardFixtures.full.utf8)
        let normalized = try LlmuxDashboardWireNormalizer.normalize(
            original,
            localEmailAnonymous: true
        )

        XCTAssertEqual(normalized.dashboardJSON, original)
        XCTAssertTrue(normalized.serverOwnsEmailSetting)
        XCTAssertEqual(
            try decode(String(decoding: normalized.dashboardJSON, as: UTF8.self)).emailAnonymous,
            false
        )
    }

    func testModelUsageRowWithoutAdditiveFieldsDecodes() throws {
        // A model row exactly as the first model-usage daemons wrote it: no
        // in_flight / accounts / efforts / endpoints / cache / cost_usd.
        let json = """
        [{"group": "claude", "model": "claude-opus-4-8", "requests": 5, "ok": 5,
          "errors": 0, "tokens_in": 10, "tokens_out": 20, "last_used_ms": 1783146111455}]
        """
        let rows = try JSONDecoder().decode([LlmuxDashboardModelUsage].self, from: Data(json.utf8))
        XCTAssertEqual(rows[0].inFlight, 0)
        XCTAssertTrue(rows[0].accounts.isEmpty)
        XCTAssertTrue(rows[0].efforts.isEmpty)
        XCTAssertTrue(rows[0].endpoints.isEmpty)
        XCTAssertNil(rows[0].cacheRead)      // unavailable ≠ 0 — renders as "—"
        XCTAssertNil(rows[0].cacheCreation)
        XCTAssertNil(rows[0].costUsd)
        XCTAssertEqual(rows[0].totalTokens, 30)
    }

    // MARK: - (c) broken payloads remain failures

    func testGarbageBodyThrows() {
        XCTAssertThrowsError(try decode("upstream exploded: not json"))
    }

    func testMissingRequiredKeysThrows() {
        // A 2xx status-shaped body is a protocol error. Compatibility is
        // selected only by an explicit unsupported dashboard HTTP status.
        XCTAssertThrowsError(try decode(#"{"accounts": [], "current": null, "port": 3456}"#))
    }

    func testWrongTypesThrow() {
        XCTAssertThrowsError(try decode(#"{"version": "x", "port": "not-a-number"}"#))
    }

    // MARK: - status-record DTO parity helper

    func testStatusRecordBridgePreservesTileFields() throws {
        let dash = try decode(DashboardFixtures.full)
        let account = try XCTUnwrap(dash.accounts.first)
        let record = account.statusRecord

        XCTAssertEqual(record.name, account.name)
        XCTAssertEqual(record.type, account.type)
        XCTAssertNil(record.group)  // dashboard doc carries no group key
        XCTAssertEqual(record.status, account.status)
        XCTAssertEqual(record.fiveHour?.utilization, account.fiveHour?.utilization)
        XCTAssertEqual(record.fiveHour?.resetsInSecs, account.fiveHour?.resetsInSecs)
        XCTAssertEqual(record.sevenDay?.utilization, account.sevenDay?.utilization)
        XCTAssertEqual(record.fableWeekly?.utilization, account.fableWeekly?.utilization)
        XCTAssertEqual(record.fableWeekly?.severity, account.fableWeekly?.severity)
        XCTAssertEqual(record.fableWeekly?.isActive, account.fableWeekly?.isActive)
        XCTAssertEqual(record.fableWeekly?.constraining, account.fableWeekly?.constraining)
        XCTAssertEqual(record.inFlight, account.inFlight)
        XCTAssertEqual(record.tokenExpiresAtMs, account.tokenExpiresAtMs)
    }

    func testCodexClassificationSurvivesMissingGroup() throws {
        // The provider split must key off `type == "codex"` when `group` is
        // absent — same derivation the daemon uses (credential kind → group).
        let dash = try decode(DashboardFixtures.full)
        let codex = dash.accounts.filter { $0.type.lowercased() == "codex" }
        XCTAssertEqual(codex.count, 2)
        for account in codex {
            XCTAssertNil(account.statusRecord.group)
            XCTAssertEqual(account.statusRecord.type, "codex")
        }
    }
}
