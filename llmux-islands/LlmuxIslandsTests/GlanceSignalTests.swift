import XCTest

// exception-beacon resolver rules (trinity consensus MUST-FIX): single worst
// chip, fixed priority, LIMIT = actual exhaustion only, window always named,
// stable tie-breaks, asymmetric degraded debounce, offline ≠ session 0.
final class GlanceSignalTests: XCTestCase {
    private func record(
        _ name: String,
        status: String = "ok",
        fiveHour: Double? = nil,
        sevenDay: Double? = nil,
        fiveResets: Int? = nil
    ) -> LlmuxAccountRecord {
        LlmuxAccountRecord(
            name: name,
            type: "oauth",
            group: "claude",
            status: status,
            fiveHour: fiveHour.map { LlmuxWindow(utilization: $0, resetsInSecs: fiveResets) },
            sevenDay: sevenDay.map { LlmuxWindow(utilization: $0, resetsInSecs: nil) },
            fableWeekly: nil,
            inFlight: 0,
            tokenExpiresAtMs: nil,
            paused: false
        )
    }

    private func resolve(
        _ records: [LlmuxAccountRecord],
        offline: Bool = false,
        degradedStreak: Int = 0
    ) -> GlanceResolver.Output {
        GlanceResolver.resolve(records: records, offline: offline, degradedStreak: degradedStreak)
    }

    // MARK: priority

    func testHealthyIsCompletelyQuiet() {
        let output = resolve([record("a", fiveHour: 0.42, sevenDay: 0.10)])
        XCTAssertEqual(output.signal, .none)
        XCTAssertNil(output.signal.chipText, "healthy label must be pixel-identical")
        XCTAssertTrue(output.attention.isEmpty)
    }

    func testOfflineBeatsEverythingAndCarriesNoStaleState() {
        let output = resolve(
            [record("a", status: "auth_failed")],
            offline: true
        )
        XCTAssertEqual(output.signal, .offline)
        XCTAssertTrue(output.attention.isEmpty, "offline shows no per-account rows")
    }

    func testAuthBeatsLimitBeatsLowQuota() {
        let output = resolve([
            record("low", fiveHour: 0.93),
            record("gone", fiveHour: 1.0),
            record("broken", status: "auth_failed"),
        ])
        XCTAssertEqual(output.signal, .auth(count: 1))
        // Every problem still reaches the open panel, auth first.
        XCTAssertEqual(output.attention.first?.account, "broken")
        XCTAssertEqual(output.attention.count, 3)
    }

    // MARK: LIMIT — actual exhaustion only, N = affected accounts

    func testLimitCountsAccountsNotWindows() {
        let output = resolve([
            record("both", fiveHour: 1.0, sevenDay: 1.0),
            record("fine", fiveHour: 0.2),
        ])
        XCTAssertEqual(output.signal, .limit(count: 1))
        XCTAssertEqual(output.signal.chipText, "LIMIT 1")
    }

    func testNinetySevenPercentIsLowQuotaNotLimit() {
        // 97% used = 3% left: unusable soon, but NOT exhausted — the chip must
        // say "5h 3%", never "LIMIT" (estimating exhaustion is forbidden).
        let output = resolve([record("hot", fiveHour: 0.97, fiveResets: 6120)])
        XCTAssertEqual(
            output.signal,
            .lowQuota(window: "5h", remainingPct: 3, account: "hot")
        )
        XCTAssertEqual(output.signal.chipText, "5h 3%")
        XCTAssertEqual(output.attention.first?.detail, "resets in 1h 42m")
    }

    // MARK: lowQuota — worst across every account × {5h, 7d}, window named

    func testLowQuotaPicksTheWorstWindowAcrossAccounts() {
        let output = resolve([
            record("a", fiveHour: 0.91),
            record("b", fiveHour: 0.40, sevenDay: 0.96),
        ])
        XCTAssertEqual(
            output.signal,
            .lowQuota(window: "7d", remainingPct: 4, account: "b")
        )
    }

    func testElevenPercentRemainingIsQuiet() {
        let output = resolve([record("a", fiveHour: 0.89)])
        XCTAssertEqual(output.signal, .none)
    }

    func testTieBreakPrefersFiveHourThenAccountName() {
        // Same remaining on both: 5h wins over 7d; equal windows fall back to
        // the account name, so the beacon can never flap between polls.
        let fiveVsSeven = resolve([
            record("a", sevenDay: 0.92),
            record("b", fiveHour: 0.92),
        ])
        XCTAssertEqual(
            fiveVsSeven.signal,
            .lowQuota(window: "5h", remainingPct: 8, account: "b")
        )
        let sameWindow = resolve([
            record("zed", fiveHour: 0.92),
            record("abe", fiveHour: 0.92),
        ])
        XCTAssertEqual(
            sameWindow.signal,
            .lowQuota(window: "5h", remainingPct: 8, account: "abe")
        )
    }

    // MARK: degraded — explicit + debounced, unknown/cold never promoted

    func testDegradedNeedsTheDebounceOnTheClosedChip() {
        let records = [record("a", status: "degraded")]
        XCTAssertEqual(resolve(records, degradedStreak: 1).signal, .none)
        XCTAssertEqual(resolve(records, degradedStreak: 2).signal, .none)
        XCTAssertEqual(
            resolve(records, degradedStreak: GlanceResolver.degradedDebounce).signal,
            .degraded
        )
        // The OPEN panel lists it immediately, debounce or not.
        XCTAssertEqual(resolve(records, degradedStreak: 1).attention.count, 1)
    }

    func testUnknownStatusesAreNeverPromoted() {
        let output = resolve(
            [record("a", status: "cooldown"), record("b")],
            degradedStreak: 99
        )
        XCTAssertEqual(output.signal, .none)
        XCTAssertTrue(output.attention.isEmpty)
    }

    func testNonOperationalStatusesNeverPromoteTheirQuota() {
        // A cooldown/unknown/paused account may carry a real window, but its
        // quota is not an actionable beacon — never LIMIT, never lowQuota.
        XCTAssertEqual(
            resolve([record("cooling", status: "cooldown", fiveHour: 1.0)]).signal,
            .none
        )
        XCTAssertEqual(
            resolve([record("weird", status: "mystery", fiveHour: 0.97)]).signal,
            .none
        )
        var paused = record("paused", fiveHour: 0.95)
        paused = LlmuxAccountRecord(
            name: paused.name, type: paused.type, group: paused.group,
            status: paused.status, fiveHour: paused.fiveHour, sevenDay: paused.sevenDay,
            fableWeekly: nil, inFlight: 0, tokenExpiresAtMs: nil, paused: true
        )
        XCTAssertEqual(resolve([paused]).signal, .none)
    }

    func testAttentionRowsAlwaysCarryAnAction() {
        // LIMIT with no reset time still tells the user what to do.
        let noReset = resolve([record("gone", fiveHour: 1.0)])
        XCTAssertEqual(noReset.attention.first?.detail, "reset unknown — rotate accounts")
        let degraded = resolve([record("a", status: "degraded")], degradedStreak: 1)
        XCTAssertEqual(degraded.attention.first?.detail, "reported by daemon — check llmux logs")
    }

    // MARK: display-name mapping (demo mode)

    func testDisplayNameMapsAttentionAndSignalAccounts() {
        let output = GlanceResolver.resolve(
            records: [record("real@secret.io", fiveHour: 0.93)],
            offline: false,
            degradedStreak: 0,
            displayName: { index, _ in "user\(index)@example.com" }
        )
        XCTAssertEqual(
            output.signal,
            .lowQuota(window: "5h", remainingPct: 7, account: "user0@example.com")
        )
        XCTAssertEqual(output.attention.first?.account, "user0@example.com")
    }

    // MARK: duration formatting

    func testDurationFormatting() {
        XCTAssertEqual(GlanceResolver.duration(50), "50s")
        XCTAssertEqual(GlanceResolver.duration(42 * 60), "42m")
        XCTAssertEqual(GlanceResolver.duration(6120), "1h 42m")
        XCTAssertEqual(GlanceResolver.duration(3 * 24 * 3600), "3d 0h")
    }
}
