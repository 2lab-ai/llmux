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

    // MARK: - Client name shortening (activity client-name)

    func testClientNameLabelFirstTokenFirstFourChars() {
        // The user-spec examples, verbatim (mirrors Rust `client_short_name`).
        XCTAssertEqual(ClientNameLabel.short("Z (U09F1M5MML1)"), "Z")
        XCTAssertEqual(ClientNameLabel.short("luka (U0AAAAAAAAA)"), "luka")
        XCTAssertEqual(ClientNameLabel.short("angelo (U0BBBBBBBBB)"), "ange")
        XCTAssertEqual(ClientNameLabel.short("local"), "loca")
    }

    func testClientNameLabelUnicodeAndEmpty() {
        // Character (grapheme) clipping — the SAME unit as Rust's
        // `client_short_name` (unicode-segmentation): a ZWJ family emoji is
        // one Character and survives whole; Hangul keeps whole syllables.
        XCTAssertEqual(ClientNameLabel.short("위대한이름 (U1)"), "위대한이")
        XCTAssertEqual(ClientNameLabel.short("한글"), "한글")
        XCTAssertEqual(ClientNameLabel.short("👨‍👩‍👧‍👦 (U1)"), "👨‍👩‍👧‍👦")
        XCTAssertEqual(ClientNameLabel.short("👨‍👩‍👧‍👦x둘셋넷 (U1)"), "👨‍👩‍👧‍👦x둘셋")
        XCTAssertEqual(ClientNameLabel.short(""), "")
        XCTAssertEqual(ClientNameLabel.short("   "), "")
    }

    // MARK: - Row label composition (activity client-name)

    private func completedRow(_ json: String) throws -> LlmuxDashboardCompleted {
        try JSONDecoder().decode(LlmuxDashboardCompleted.self, from: Data(json.utf8))
    }

    func testCompletedLabelLeadsWithShortClientName() throws {
        // The PRODUCTION composition — dropping the name from the label
        // makes this fail, not just the shortening helper.
        let named = try completedRow(
            #"{"kind":"request","at_ms":1,"model":"claude-opus-4-8","client_name":"Z (U09F1M5MML1)"}"#
        )
        XCTAssertEqual(ClientNameLabel.completedLabel(named), "Z claude-opus-4-8")
        let bare = try completedRow(#"{"kind":"request","at_ms":1,"model":"claude-opus-4-8"}"#)
        XCTAssertEqual(ClientNameLabel.completedLabel(bare), "claude-opus-4-8")
        // Whitespace-only name shortens to empty → bare label, never a
        // leading space (guard sits on the SHORTENED value).
        let blank = try completedRow(
            #"{"kind":"request","at_ms":1,"model":"claude-opus-4-8","client_name":"   "}"#
        )
        XCTAssertEqual(ClientNameLabel.completedLabel(blank), "claude-opus-4-8")
    }

    func testActivityRowModelComposesTheDashboardRowVerbatim() throws {
        // The factory output IS what UsageActivityList renders (the view has
        // no label/trailing expression of its own) — dropping the short name
        // from the composed row fails HERE, at the production surface.
        let entry = try completedRow(
            #"{"kind":"request","at_ms":0,"model":"claude-opus-4-8","client_name":"Z (U09F1M5MML1)","status":200,"duration_ms":1000,"tokens":{"input":10,"output":5}}"#
        )
        let now = Date(timeIntervalSince1970: 300)
        let model = ActivityRowModel.completed(entry, now: now)
        XCTAssertEqual(model.label, "Z claude-opus-4-8")
        XCTAssertEqual(model.status, 200)
        XCTAssertNil(model.marker)
        XCTAssertEqual(model.time, "5m")
        XCTAssertEqual(model.trailing, "10→5  1.0s")
    }

    func testActivityRowModelComposesReceiptRowsForAllKinds() throws {
        let now = Date(timeIntervalSince1970: 300)
        // Request receipt: short name leads the label (the canonical list
        // renders this model verbatim).
        let requestJSON = #"{"receipt_id":"request:1:POST:/v1/messages:200","kind":"request","occurred_at_ms":0,"status":200,"method":"POST","path":"/v1/messages","fast":false,"error":false,"model":"claude-opus-4-8","client_name":"angelo (U0B)","duration_ms":1000}"#
        let request = try JSONDecoder().decode(SharedActivityReceipt.self, from: Data(requestJSON.utf8))
        let requestModel = ActivityRowModel.receipt(request, now: now)
        XCTAssertEqual(requestModel.label, "ange claude-opus-4-8")
        XCTAssertNil(requestModel.marker)
        XCTAssertEqual(requestModel.trailing, "1.0s")
        // In-flight receipt: marker + elapsed trailing, no name invented.
        let inFlightJSON = #"{"receipt_id":"in_flight:9","kind":"in_flight","occurred_at_ms":0,"method":"POST","path":"/v1/messages","fast":false,"error":false,"elapsed_ms":2000}"#
        let inFlight = try JSONDecoder().decode(SharedActivityReceipt.self, from: Data(inFlightJSON.utf8))
        let inFlightModel = ActivityRowModel.receipt(inFlight, now: now)
        XCTAssertEqual(inFlightModel.marker, "···")
        XCTAssertEqual(inFlightModel.label, "/v1/messages")
        XCTAssertEqual(inFlightModel.trailing, "2.0s")
        // Note receipt: marker "note", message verbatim as the label.
        let noteJSON = #"{"receipt_id":"note:1:0","kind":"note","occurred_at_ms":0,"fast":false,"error":false,"message":"token refreshed: anonymous"}"#
        let note = try JSONDecoder().decode(SharedActivityReceipt.self, from: Data(noteJSON.utf8))
        let noteModel = ActivityRowModel.receipt(note, now: now)
        XCTAssertEqual(noteModel.marker, "note")
        XCTAssertEqual(noteModel.label, "token refreshed: anonymous")
    }

    func testReceiptLabelLeadsWithShortClientName() throws {
        // Canonical shared-core receipt path (the list the app prefers when
        // receipts exist) uses the same tested composition.
        let json = #"{"receipt_id":"request:1:POST:/v1/messages:200","kind":"request","occurred_at_ms":1,"status":200,"method":"POST","path":"/v1/messages","fast":false,"error":false,"model":"claude-opus-4-8","client_name":"angelo (U0B)"}"#
        let receipt = try JSONDecoder().decode(SharedActivityReceipt.self, from: Data(json.utf8))
        XCTAssertEqual(ClientNameLabel.receiptLabel(receipt), "ange claude-opus-4-8")
        // Notes keep their message verbatim (no name on note receipts).
        let noteJSON = #"{"receipt_id":"note:1:0","kind":"note","occurred_at_ms":1,"fast":false,"error":false,"message":"token refreshed: anonymous"}"#
        let note = try JSONDecoder().decode(SharedActivityReceipt.self, from: Data(noteJSON.utf8))
        XCTAssertEqual(ClientNameLabel.receiptLabel(note), "token refreshed: anonymous")
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
        // Pure legacy-DTO parity; the live adapter delegates this derivation
        // to Rust after normalizing the status document.
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
