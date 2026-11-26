import XCTest

/// NEEDS ATTENTION `_` (to the bottom, stays there; only new rows come back
/// on top) and `x` (dismiss the rows of that group).
final class AttentionTriageTests: XCTestCase {
    private func record(_ name: String, status: String = "ok", sevenDay: Double? = nil) -> LlmuxAccountRecord {
        LlmuxAccountRecord(
            name: name,
            type: "oauth",
            group: "claude",
            status: status,
            sevenDay: sevenDay.map { LlmuxWindow(utilization: $0, resetsInSecs: 3600) },
            fableWeekly: nil,
            inFlight: 0,
            tokenExpiresAtMs: nil,
            paused: false
        )
    }

    private func rows(_ records: [LlmuxAccountRecord]) -> [GlanceAttention] {
        GlanceResolver.resolve(records: records, offline: false, degradedStreak: 0).attention
    }

    func testResolverNamesEachCondition() {
        let kinds = rows([
            record("a", status: "auth_failed"),
            record("b", sevenDay: 1.0),
            record("c", sevenDay: 0.95),
            record("d", status: "degraded"),
        ]).map(\.kind)
        XCTAssertEqual(kinds, ["auth", "limit:7d", "low:7d", "degraded"])
    }

    func testMinimizedRowsStayAtTheBottomAndOnlyNewRowsShowOnTop() {
        var triage = AttentionTriage()
        let first = rows([record("a", sevenDay: 1.0), record("b", status: "auth_failed")])
        triage.minimize(triage.split(first).top)
        XCTAssertTrue(triage.split(first).top.isEmpty)
        XCTAssertEqual(triage.split(first).bottom.map(\.account), ["b", "a"])

        // A later poll adds one problem: only that one is on top.
        let second = rows([
            record("a", sevenDay: 1.0),
            record("b", status: "auth_failed"),
            record("c", sevenDay: 1.0),
        ])
        triage.observe(second, online: true)
        XCTAssertEqual(triage.split(second).top.map(\.account), ["c"])
        XCTAssertEqual(triage.split(second).bottom.map(\.account), ["b", "a"])
    }

    func testAMinimizedRowStaysDownWhileItsNumbersMove() {
        var triage = AttentionTriage()
        let before = rows([record("a", sevenDay: 0.92)]) // "7d 8% left"
        triage.minimize(before)
        let after = rows([record("a", sevenDay: 0.94)]) // "7d 6% left"
        XCTAssertNotEqual(before.first?.reason, after.first?.reason)
        XCTAssertTrue(triage.split(after).top.isEmpty)
        XCTAssertEqual(triage.split(after).bottom.count, 1)
    }

    func testAWorseConditionIsNew() {
        var triage = AttentionTriage()
        triage.minimize(rows([record("a", sevenDay: 0.95)]))
        // Low quota became exhaustion: a different action, so it comes back on top.
        let worse = rows([record("a", sevenDay: 1.0)])
        XCTAssertEqual(triage.split(worse).top.map(\.kind), ["limit:7d"])
    }

    func testDismissHidesTheRowsUntilTheConditionClearsAndReturns() {
        var triage = AttentionTriage()
        let current = rows([record("a", sevenDay: 1.0), record("b", sevenDay: 1.0)])
        triage.dismiss(current)
        XCTAssertTrue(triage.split(current).top.isEmpty)
        XCTAssertTrue(triage.split(current).bottom.isEmpty)

        // Missing from a few polls (an account cold after a daemon restart),
        // then back: still dismissed, and the absence count starts over.
        for _ in 0..<10 { triage.observe([], online: true) }
        triage.observe(current, online: true)
        XCTAssertTrue(triage.split(current).top.isEmpty)

        // Missing from every poll for the whole grace period: forgotten, so it
        // is new the next time it appears.
        for _ in 0..<(AttentionTriage.forgetAfterAbsentPolls - 1) { triage.observe([], online: true) }
        XCTAssertTrue(triage.split(current).top.isEmpty, "one poll short of the grace period")
        XCTAssertTrue(triage.observe([], online: true))
        XCTAssertEqual(triage.split(current).top.count, 2)
    }

    func testOfflinePollsNeverUndoADismissal() {
        var triage = AttentionTriage()
        let current = rows([record("a", sevenDay: 1.0)])
        triage.dismiss(current)
        // A long daemon outage: every poll is offline with no rows.
        for _ in 0..<(AttentionTriage.forgetAfterAbsentPolls * 2) {
            XCTAssertFalse(triage.observe([], online: false))
        }
        XCTAssertTrue(triage.split(current).top.isEmpty, "still dismissed after reconnecting")
    }

    func testMaskedNamesDoNotShareATriageEntry() {
        // Email anonymity can show two accounts with the same text; the
        // account id keeps their rows apart.
        let masked = "a***@example.com"
        let first = GlanceAttention(account: masked, reason: "authentication required — re-login",
                                    detail: nil, kind: "auth", identity: "handle-1")
        let second = GlanceAttention(account: masked, reason: "authentication required — re-login",
                                     detail: nil, kind: "auth", identity: "handle-2")
        var triage = AttentionTriage()
        triage.dismiss([first])
        XCTAssertEqual(triage.split([first, second]).top, [second])
    }

    func testMaskedQuotaRowsStayApartThroughTheResolver() {
        // Two accounts that email anonymity shows with the same text, for
        // both quota conditions: acting on one must not touch the other.
        let masked: (Int, String) -> String = { _, _ in "a***@example.com" }
        let ids: (Int, String) -> String = { index, _ in "id-\(index)" }
        for usage in [0.95, 1.0] {
            let rows = GlanceResolver.resolve(
                records: [record("first", sevenDay: usage), record("second", sevenDay: usage)],
                offline: false,
                degradedStreak: 0,
                displayName: masked,
                identity: ids
            ).attention
            XCTAssertEqual(rows.count, 2)
            XCTAssertEqual(Set(rows.map(\.account)).count, 1, "names collide on purpose")

            var minimized = AttentionTriage()
            minimized.minimize([rows[0]])
            XCTAssertEqual(minimized.split(rows).top, [rows[1]], "usage \(usage)")
            XCTAssertEqual(minimized.split(rows).bottom, [rows[0]], "usage \(usage)")

            var dismissed = AttentionTriage()
            dismissed.dismiss([rows[1]])
            XCTAssertEqual(dismissed.split(rows).top, [rows[0]], "usage \(usage)")
        }
    }

    func testResolverCarriesTheAccountIdentity() {
        let output = GlanceResolver.resolve(
            records: [record("shown", status: "auth_failed")],
            offline: false,
            degradedStreak: 0,
            identity: { index, _ in "id-\(index)" }
        )
        XCTAssertEqual(output.attention.first?.identity, "id-0")
    }

    func testPersistsDigestsNotAccountNames() throws {
        let suite = "AttentionTriageTests.\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }

        var triage = AttentionTriage()
        let current = rows([record("someone@example.com", sevenDay: 1.0)])
        triage.minimize(current)
        triage.save(to: defaults)

        let stored = try XCTUnwrap(defaults.data(forKey: AttentionTriage.defaultsKey))
        XCTAssertFalse(String(decoding: stored, as: UTF8.self).contains("someone"))
        XCTAssertEqual(AttentionTriage.load(from: defaults), triage)
        XCTAssertEqual(AttentionTriage.load(from: defaults).split(current).bottom.count, 1)
    }
}
