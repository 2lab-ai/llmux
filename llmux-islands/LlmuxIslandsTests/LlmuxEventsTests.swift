import XCTest

/// Logic tests for the llmux events list (canned JSON/strings only — no
/// network): tolerant event decode (absent / empty / partial), local-time
/// rendering of BOTH timestamp formats (compact `YYYYMMDDHHMM` local and
/// RFC3339-with-offset), the active-window predicate (`from <= now < to`),
/// and the upsert/delete echo-list decoding.
final class LlmuxEventsTests: XCTestCase {
    /// All time assertions pin Seoul (+09:00, no DST) so they are exact and
    /// machine-independent.
    private let seoul = TimeZone(identifier: "Asia/Seoul")!

    private func date(_ iso: String) -> Date {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime]
        return f.date(from: iso)!
    }

    // MARK: - Event decode: absent / empty / partial

    func testDashboardWithoutEventsDecodesEmpty() throws {
        // A minimal pre-events dashboard doc — the additive field must decode
        // as [] exactly like the other additive dashboard fields.
        let json = """
        {"version": "llmux 0.2.14", "port": 3456, "uptime_secs": 1,
         "accounts": [],
         "totals": {"requests": 0, "ok": 0, "errors": 0, "tokens_in": 0, "tokens_out": 0},
         "activity": {"in_flight": [], "completed": []}}
        """
        let dash = try JSONDecoder().decode(LlmuxDashboard.self, from: Data(json.utf8))
        XCTAssertEqual(dash.events, [])
    }

    func testDashboardWithEventsDecodes() throws {
        let json = """
        {"version": "llmux 0.2.17", "port": 3456, "uptime_secs": 1,
         "accounts": [],
         "totals": {"requests": 0, "ok": 0, "errors": 0, "tokens_in": 0, "tokens_out": 0},
         "activity": {"in_flight": [], "completed": []},
         "events": [{"id": "20260712-fable5", "from": "202607080000",
                     "to": "202607130000", "content": "Fable 5 Available until 7/12"}]}
        """
        let dash = try JSONDecoder().decode(LlmuxDashboard.self, from: Data(json.utf8))
        XCTAssertEqual(dash.events, [
            LlmuxEvent(id: "20260712-fable5", from: "202607080000",
                       to: "202607130000", content: "Fable 5 Available until 7/12"),
        ])
    }

    func testDashboardWithEmptyEventsDecodesEmpty() throws {
        let json = """
        {"version": "llmux 0.2.17", "port": 3456, "uptime_secs": 1,
         "accounts": [],
         "totals": {"requests": 0, "ok": 0, "errors": 0, "tokens_in": 0, "tokens_out": 0},
         "activity": {"in_flight": [], "completed": []},
         "events": []}
        """
        let dash = try JSONDecoder().decode(LlmuxDashboard.self, from: Data(json.utf8))
        XCTAssertEqual(dash.events, [])
    }

    func testPartialEventDecodesWithEmptyDefaults() throws {
        // A partial object must not fail the whole document — missing keys
        // default to "" (additive-field rule).
        let events = try JSONDecoder().decode(
            [LlmuxEvent].self,
            from: Data(#"[{"id": "x"}, {}]"#.utf8)
        )
        XCTAssertEqual(events, [
            LlmuxEvent(id: "x", from: "", to: "", content: ""),
            LlmuxEvent(id: "", from: "", to: "", content: ""),
        ])
    }

    // MARK: - Echo list decode (`POST /llmux/events` responses)

    func testEchoDecodesCanonicalShape() {
        // The contract shape: {"ok": true, "events": [<stored list>]}.
        let data = Data(#"{"ok": true, "events": [{"id": "a", "from": "202607080000", "to": "202607130000", "content": "c"}]}"#.utf8)
        XCTAssertEqual(LlmuxEventList.decode(data), [
            LlmuxEvent(id: "a", from: "202607080000", to: "202607130000", content: "c"),
        ])
        XCTAssertEqual(LlmuxEventList.decode(Data(#"{"ok": true, "events": []}"#.utf8)), [])
        // `events` omitted when empty (dashboard omit-when-empty rule) → [].
        XCTAssertEqual(LlmuxEventList.decode(Data(#"{"ok": true}"#.utf8)), [])
    }

    func testEchoToleratesBareArray() {
        let data = Data(#"[{"id": "a", "from": "f", "to": "t", "content": "c"}]"#.utf8)
        XCTAssertEqual(LlmuxEventList.decode(data), [
            LlmuxEvent(id: "a", from: "f", to: "t", content: "c"),
        ])
        XCTAssertEqual(LlmuxEventList.decode(Data("[]".utf8)), [])
    }

    func testEchoUnrecognizedShapeIsNil() {
        XCTAssertNil(LlmuxEventList.decode(Data(#"{"error": "bad"}"#.utf8)))
        XCTAssertNil(LlmuxEventList.decode(Data("not json".utf8)))
        XCTAssertNil(LlmuxEventList.decode(Data()))
    }

    // MARK: - Timestamp parsing

    func testParseCompactIsLocalWallClock() {
        // 202607080000 in Seoul = 2026-07-07T15:00:00Z.
        XCTAssertEqual(
            LlmuxEventTime.parse("202607080000", timeZone: seoul),
            date("2026-07-07T15:00:00Z")
        )
        // The same wall-clock string in UTC is a DIFFERENT instant — the zone
        // parameter is load-bearing.
        XCTAssertEqual(
            LlmuxEventTime.parse("202607080000", timeZone: TimeZone(identifier: "UTC")!),
            date("2026-07-08T00:00:00Z")
        )
    }

    func testParseRFC3339WithOffset() {
        XCTAssertEqual(
            LlmuxEventTime.parse("2026-07-12T23:30:00+09:00", timeZone: seoul),
            date("2026-07-12T14:30:00Z")
        )
        // Fractional seconds tolerated.
        let fractional = LlmuxEventTime.parse("2026-07-12T23:30:00.500+09:00", timeZone: seoul)
        XCTAssertNotNil(fractional)
        XCTAssertEqual(
            fractional!.timeIntervalSince1970,
            date("2026-07-12T14:30:00Z").timeIntervalSince1970 + 0.5,
            accuracy: 0.001
        )
    }

    func testParseGarbageIsNil() {
        XCTAssertNil(LlmuxEventTime.parse("", timeZone: seoul))
        XCTAssertNil(LlmuxEventTime.parse("tomorrow", timeZone: seoul))
        XCTAssertNil(LlmuxEventTime.parse("2026070800", timeZone: seoul))      // 10 digits
        XCTAssertNil(LlmuxEventTime.parse("20260708000000", timeZone: seoul)) // 14 digits
        XCTAssertNil(LlmuxEventTime.parse("202613080000", timeZone: seoul))   // month 13
        XCTAssertNil(LlmuxEventTime.parse("202602300000", timeZone: seoul))   // Feb 30
        XCTAssertNil(LlmuxEventTime.parse("202607082400", timeZone: seoul))   // hour 24
    }

    // MARK: - Local-time rendering

    func testDisplayRendersBothFormatsInLocalTime() {
        // Compact input is already local wall-clock — renders verbatim time.
        XCTAssertEqual(LlmuxEventTime.display("202607080000", timeZone: seoul), "7/8 00:00")
        // RFC3339 with a matching offset.
        XCTAssertEqual(LlmuxEventTime.display("2026-07-12T23:30:00+09:00", timeZone: seoul), "7/12 23:30")
        // RFC3339 in UTC crosses midnight when rendered in Seoul.
        XCTAssertEqual(LlmuxEventTime.display("2026-07-12T23:30:00Z", timeZone: seoul), "7/13 08:30")
    }

    func testDisplayGarbageInVerbatimOut() {
        XCTAssertEqual(LlmuxEventTime.display("not-a-time", timeZone: seoul), "not-a-time")
        XCTAssertEqual(LlmuxEventTime.display("", timeZone: seoul), "")
    }

    func testDisplayRange() {
        XCTAssertEqual(
            LlmuxEventTime.displayRange(from: "202607080000", to: "202607130000", timeZone: seoul),
            "7/8 00:00 → 7/13 00:00"
        )
    }

    // MARK: - Active-window predicate: from <= now < to

    func testIsActiveInsideWindow() {
        XCTAssertTrue(LlmuxEventTime.isActive(
            from: "202607080000", to: "202607130000",
            now: date("2026-07-10T00:00:00Z"), timeZone: seoul
        ))
    }

    func testIsActiveBoundsFromInclusiveToExclusive() {
        // Exactly at `from` (2026-07-08 00:00 KST = 07-07T15:00Z) → active.
        XCTAssertTrue(LlmuxEventTime.isActive(
            from: "202607080000", to: "202607130000",
            now: date("2026-07-07T15:00:00Z"), timeZone: seoul
        ))
        // Exactly at `to` (2026-07-13 00:00 KST = 07-12T15:00Z) → NOT active.
        XCTAssertFalse(LlmuxEventTime.isActive(
            from: "202607080000", to: "202607130000",
            now: date("2026-07-12T15:00:00Z"), timeZone: seoul
        ))
    }

    func testIsActiveOutsideWindowOrUnparseable() {
        // Before the window.
        XCTAssertFalse(LlmuxEventTime.isActive(
            from: "202607080000", to: "202607130000",
            now: date("2026-07-01T00:00:00Z"), timeZone: seoul
        ))
        // Mixed formats work together.
        XCTAssertTrue(LlmuxEventTime.isActive(
            from: "2026-07-08T00:00:00+09:00", to: "202607130000",
            now: date("2026-07-10T00:00:00Z"), timeZone: seoul
        ))
        // Unparseable bounds are never active.
        XCTAssertFalse(LlmuxEventTime.isActive(
            from: "", to: "202607130000",
            now: date("2026-07-10T00:00:00Z"), timeZone: seoul
        ))
        XCTAssertFalse(LlmuxEventTime.isActive(
            from: "202607080000", to: "garbage",
            now: date("2026-07-10T00:00:00Z"), timeZone: seoul
        ))
    }
}
