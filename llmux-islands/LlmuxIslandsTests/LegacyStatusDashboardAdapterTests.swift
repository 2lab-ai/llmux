import XCTest

final class LegacyStatusDashboardAdapterTests: XCTestCase {
    func testStatusIsCompletedIntoDashboardShapeForRust() throws {
        let status = Data(#"""
        {
          "version": "llmux 0.1.9",
          "port": 4567,
          "current": "codex:user@example.com",
          "accounts": [{
            "name": "codex:user@example.com",
            "type": "codex",
            "group": "codex",
            "status": "active",
            "five_hour": {"utilization": 0.25, "resets_in_secs": 300},
            "seven_day": null,
            "in_flight": 2
          }]
        }
        """#.utf8)

        let data = try LegacyStatusDashboardAdapter.dashboardData(
            from: status,
            previousDashboardData: nil,
            endpointPort: 3456,
            receivedAtMs: 1_000_000,
            emailAnonymous: true,
            showFableWeekly: false
        )
        let dashboard = try XCTUnwrap(
            try JSONSerialization.jsonObject(with: data) as? [String: Any]
        )
        let account = try XCTUnwrap((dashboard["accounts"] as? [[String: Any]])?.first)
        let fiveHour = try XCTUnwrap(account["five_hour"] as? [String: Any])

        XCTAssertEqual(dashboard["current_by_group"] as? [String: String], ["codex": "codex:user@example.com"])
        XCTAssertEqual(dashboard["email_anonymous"] as? Bool, true)
        XCTAssertEqual(dashboard["show_fable_weekly"] as? Bool, false)
        XCTAssertEqual(account["name"] as? String, "codex:user@example.com")
        XCTAssertEqual(account["healthy"] as? Bool, true)
        XCTAssertEqual(fiveHour["resets_at"] as? Int, 1_300)
        XCTAssertEqual(fiveHour["fetched_at_ms"] as? Int, 1_000_000)
        XCTAssertNotNil(account["session"] as? [String: Any])
        XCTAssertNotNil(dashboard["scheduler"] as? [String: Any])
        XCTAssertNotNil(dashboard["activity"] as? [String: Any])
    }

    func testStatusReplacesLiveAccountsButRetainsAcceptedAnalytics() throws {
        let status = Data(#"""
        {
          "version": "llmux 0.1.9",
          "current": "claude:user1@example.com",
          "accounts": [{
            "name": "claude:user1@example.com",
            "type": "oauth",
            "group": "claude",
            "status": "active",
            "five_hour": {"utilization": 0.2, "resets_at": 2000, "resets_in_secs": 1000},
            "seven_day": {"utilization": 0.3, "resets_at": 3000, "resets_in_secs": 2000},
            "in_flight": 3
          }]
        }
        """#.utf8)

        let data = try LegacyStatusDashboardAdapter.dashboardData(
            from: status,
            previousDashboardData: Data(DashboardFixtures.full.utf8),
            endpointPort: 3456,
            receivedAtMs: 1_000_000,
            emailAnonymous: true,
            showFableWeekly: true
        )
        let dashboard = try XCTUnwrap(
            try JSONSerialization.jsonObject(with: data) as? [String: Any]
        )
        let accounts = try XCTUnwrap(dashboard["accounts"] as? [[String: Any]])
        let totals = try XCTUnwrap(dashboard["totals"] as? [String: Any])
        let session = try XCTUnwrap(accounts.first?["session"] as? [String: Any])

        XCTAssertEqual(accounts.count, 1)
        XCTAssertEqual((dashboard["model_usage"] as? [[String: Any]])?.count, 2)
        XCTAssertEqual(totals["requests"] as? Int, 36_633)
        XCTAssertEqual(totals["in_flight"] as? Int, 3)
        XCTAssertEqual(session["requests"] as? Int, 5_766)
        XCTAssertEqual(dashboard["email_anonymous"] as? Bool, true)
    }

    func testMalformedStatusIsRejectedInsteadOfBypassingRust() {
        XCTAssertThrowsError(try LegacyStatusDashboardAdapter.dashboardData(
            from: Data(#"{"accounts":[{"name":"secret@example.com"}]}"#.utf8),
            previousDashboardData: nil,
            endpointPort: 3456,
            receivedAtMs: 1,
            emailAnonymous: false,
            showFableWeekly: true
        ))
    }
}
