import XCTest

/// Logic tests for the Islands analytics v1 hard rules (issue #62 S4):
/// the health-warning condition, (group, model) row identity, `—`-never-0
/// formatting, error-rate math, the closed-island summary format, and the
/// byte-identical data-quality fallbacks.
final class DashboardAnalyticsTests: XCTestCase {
    // MARK: - Error rate (U7): errors / max(requests, 1)

    func testErrorRate() {
        XCTAssertEqual(DashFormat.errorRate(errors: 1012, requests: 36633), 1012.0 / 36633.0, accuracy: 1e-12)
        // Zero requests must not divide by zero — max(requests, 1).
        XCTAssertEqual(DashFormat.errorRate(errors: 5, requests: 0), 5.0, accuracy: 1e-12)
        XCTAssertEqual(DashFormat.percent(DashFormat.errorRate(errors: 0, requests: 100)), "0.0%")
        XCTAssertEqual(DashFormat.percent(0.02763), "2.8%")
    }

    // MARK: - `—` formatting (U21 + client zeros)

    func testNilRendersUnavailableNeverZero() {
        XCTAssertEqual(DashFormat.count(nil as UInt64?), "—")
        XCTAssertEqual(DashFormat.cost(nil), "—")
        XCTAssertEqual(DashFormat.count(UInt64(0)), "0")   // a REAL server 0 stays 0
    }

    func testClientZerosAreOmitted() {
        // S1 ships wire-ready ZEROS for client cost/last-seen (#32 pending):
        // 0 means "not attributed" — the element is OMITTED (nil, issue #68),
        // never "$0.00" / a 1970 date / a `—` placeholder column.
        XCTAssertNil(DashFormat.clientCost(0))
        XCTAssertNil(DashFormat.clientCost(nil))
        XCTAssertEqual(DashFormat.clientCost(0.5), "$0.50")
        let now = Date()
        XCTAssertNil(DashFormat.clientLastSeen(0, now: now))
        XCTAssertNil(DashFormat.clientLastSeen(nil, now: now))
        let fiveMinAgo = UInt64((now.timeIntervalSince1970 - 300) * 1000)
        XCTAssertEqual(DashFormat.clientLastSeen(fiveMinAgo, now: now), "5m")
    }

    // MARK: - Client id labels (#68): raw metadata.user_id JSON → short label

    func testClientIDLabelParsesUserIdJSON() {
        // The wire shape Claude Code sends (64-hex device_id + UUIDs; fixture
        // values are synthetic — the device hash is sha256("")).
        let raw = #"{"device_id":"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","account_uuid":"00000000-0000-4000-8000-000000000000","session_id":"468acafe-0000-4000-8000-0000c0ffee00"}"#
        XCTAssertEqual(ClientIDLabel.display(raw), "e3b0c442…b855 · sess 468a")

        // No session id → device hash only, no dangling separator.
        let deviceOnly = #"{"device_id":"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"}"#
        XCTAssertEqual(ClientIDLabel.display(deviceOnly), "e3b0c442…b855")

        // A device id too short to compress renders whole.
        XCTAssertEqual(ClientIDLabel.display(#"{"device_id":"abc123"}"#), "abc123")
    }

    func testClientIDLabelGarbageInVerbatimOut() {
        // Non-JSON ids and `unknown` render as-is — never mangled.
        XCTAssertEqual(ClientIDLabel.display("unknown"), "unknown")
        XCTAssertEqual(ClientIDLabel.display("client-2"), "client-2")
        XCTAssertEqual(ClientIDLabel.display("{not json"), "{not json")
        XCTAssertEqual(ClientIDLabel.display(""), "")
        // JSON but not the user_id shape (no string device_id) → verbatim.
        XCTAssertEqual(ClientIDLabel.display(#"{"session_id":"468acafe"}"#), #"{"session_id":"468acafe"}"#)
        XCTAssertEqual(ClientIDLabel.display(#"{"device_id":42}"#), #"{"device_id":42}"#)
        XCTAssertEqual(ClientIDLabel.display(#"{"device_id":""}"#), #"{"device_id":""}"#)
        XCTAssertEqual(ClientIDLabel.display(#"["device_id"]"#), #"["device_id"]"#)
    }

    func testCountAndCostFormatting() {
        XCTAssertEqual(DashFormat.count(UInt64(999)), "999")
        XCTAssertEqual(DashFormat.count(UInt64(12_345)), "12.3k")
        XCTAssertEqual(DashFormat.count(UInt64(109_613_759)), "110M")
        XCTAssertEqual(DashFormat.cost(0.42), "$0.42")
        XCTAssertEqual(DashFormat.cost(123.4), "$123")
        XCTAssertEqual(DashFormat.cost(8689.71), "$8.7k")
        XCTAssertEqual(DashFormat.duration(ms: 320), "320ms")
        XCTAssertEqual(DashFormat.duration(ms: 9752), "9.8s")
    }

    // MARK: - Health warning (U11/U13): auth_failed OR quota > 90%

    func testQuotaRule() {
        XCTAssertTrue(DashboardHealth.isOverQuota(fiveHour: 0.91, sevenDay: nil, fableUtilization: nil, fableConstraining: nil))
        XCTAssertTrue(DashboardHealth.isOverQuota(fiveHour: nil, sevenDay: 0.95, fableUtilization: nil, fableConstraining: nil))
        // Exactly 90% is NOT "> 90%".
        XCTAssertFalse(DashboardHealth.isOverQuota(fiveHour: 0.9, sevenDay: 0.9, fableUtilization: nil, fableConstraining: nil))
        // Scoped Fable weekly counts only while the daemon marks it constraining.
        XCTAssertTrue(DashboardHealth.isOverQuota(fiveHour: nil, sevenDay: nil, fableUtilization: 0.95, fableConstraining: true))
        XCTAssertFalse(DashboardHealth.isOverQuota(fiveHour: nil, sevenDay: nil, fableUtilization: 0.95, fableConstraining: false))
        XCTAssertFalse(DashboardHealth.isOverQuota(fiveHour: nil, sevenDay: nil, fableUtilization: 0.95, fableConstraining: nil))
    }

    func testHealthSummaryFromDashboardFixture() throws {
        let dash = try JSONDecoder().decode(LlmuxDashboard.self, from: Data(DashboardFixtures.full.utf8))
        let summary = DashboardHealth.summary(dash.accounts)
        // Fixture: no auth_failed; codex:user4 has seven_day 1.0 (> 0.9);
        // claude:user1's fable 0.91 does NOT count (constraining: false).
        XCTAssertEqual(summary.authFailed, 0)
        XCTAssertEqual(summary.overQuota, 1)
        XCTAssertTrue(summary.isWarning)
        XCTAssertEqual(summary.total, 1)   // the closed pill's ⚠N count
        XCTAssertEqual(DashboardHealth.bannerText(summary), "1 account over 90% quota")
    }

    func testHealthSummaryAuthFailedOnStatusRecords() throws {
        // The status-fallback path (old daemons) applies the same rule.
        let json = """
        [{"name": "claude:a@example.com", "type": "oauth", "status": "auth_failed"},
         {"name": "claude:b@example.com", "type": "oauth", "status": "active",
          "five_hour": {"utilization": 0.2}, "seven_day": {"utilization": 0.3}}]
        """
        let records = try JSONDecoder().decode([LlmuxAccountRecord].self, from: Data(json.utf8))
        let summary = DashboardHealth.summary(records: records)
        XCTAssertEqual(summary.authFailed, 1)
        XCTAssertEqual(summary.overQuota, 0)
        XCTAssertTrue(summary.isWarning)
        XCTAssertEqual(summary.total, 1)
        XCTAssertEqual(DashboardHealth.bannerText(summary), "1 account auth failed")
    }

    // MARK: - (group, model) row identity (U13 hard rule)

    func testModelRowsKeyedByGroupAndModel() throws {
        // The same model name under claude AND codex must stay two rows.
        let json = """
        [{"group": "codex", "model": "gpt-5.5", "requests": 10, "ok": 10, "errors": 0,
          "tokens_in": 100, "tokens_out": 50, "last_used_ms": 1},
         {"group": "claude", "model": "gpt-5.5", "requests": 2, "ok": 2, "errors": 0,
          "tokens_in": 900, "tokens_out": 100, "last_used_ms": 2}]
        """
        let rows = try JSONDecoder().decode([LlmuxDashboardModelUsage].self, from: Data(json.utf8))
        XCTAssertEqual(Set(rows.map(\.id)).count, 2, "same model text in two groups = two identities")
        XCTAssertEqual(rows[0].id, "codex/gpt-5.5")
        XCTAssertEqual(rows[1].id, "claude/gpt-5.5")

        // top() sorts by total tokens without merging the two rows.
        let top = LlmuxDashboardModelUsage.top(rows, limit: 3)
        XCTAssertEqual(top.map(\.id), ["claude/gpt-5.5", "codex/gpt-5.5"])
    }

    // MARK: - Statistics sections (#68 v2)

    func testStatsSectionContract() {
        // The Statistics panel serves the four #62 analytics surfaces in this
        // exact order with these segment titles — dropping or reordering a
        // section is a regression, not a style choice.
        XCTAssertEqual(StatsSection.allCases.map(\.rawValue), ["overview", "models", "clients", "health"])
        XCTAssertEqual(StatsSection.allCases.map(\.title), ["Overview", "Models", "Clients", "Health"])
    }

    // MARK: - Data-quality labels (U19–U22)

    func testDataQualityFallbacksAreByteIdenticalToRustDefaults() {
        // Must match src/dashboard.rs serde defaults (slice S2) byte-for-byte.
        XCTAssertEqual(DataQualityFallback.modelUsage, "hydrated activity/runtime")
        XCTAssertEqual(DataQualityFallback.windowed, "best effort")
        XCTAssertEqual(DataQualityFallback.cost, "API-equivalent estimate")
        XCTAssertEqual(DataQualityFallback.cache, "missing fields shown as unavailable")

        let fallback = DataQualityLabels(nil)
        XCTAssertEqual(fallback.modelUsage, DataQualityFallback.modelUsage)
        XCTAssertEqual(fallback.windowed, DataQualityFallback.windowed)
        XCTAssertEqual(fallback.cost, DataQualityFallback.cost)
        XCTAssertEqual(fallback.cache, DataQualityFallback.cache)
    }

    func testDataQualityServerValuesWin() throws {
        let json = """
        {"model_usage": "server says A", "windowed": "server says B",
         "cost": "server says C", "cache": "server says D"}
        """
        let quality = try JSONDecoder().decode(LlmuxDashboardDataQuality.self, from: Data(json.utf8))
        let labels = DataQualityLabels(quality)
        XCTAssertEqual(labels.modelUsage, "server says A")
        XCTAssertEqual(labels.windowed, "server says B")
        XCTAssertEqual(labels.cost, "server says C")
        XCTAssertEqual(labels.cache, "server says D")
    }
}
