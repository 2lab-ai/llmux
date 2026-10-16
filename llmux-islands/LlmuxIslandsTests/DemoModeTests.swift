import XCTest

final class DemoModeTests: XCTestCase {
    func testForcedInFlightAcceptsAnySingleProviderKey() {
        XCTAssertEqual(
            DemoMode.parseForcedInFlight("grok=2"),
            DemoMode.ForcedInFlight(grok: 2)
        )
        XCTAssertEqual(
            DemoMode.parseForcedInFlight(" gemini = 4 "),
            DemoMode.ForcedInFlight(gemini: 4)
        )
        XCTAssertEqual(
            DemoMode.parseForcedInFlight("claude=3,codex=2"),
            DemoMode.ForcedInFlight(claude: 3, codex: 2)
        )
    }

    func testForcedInFlightIsNilWhenNoProviderKeyParses() {
        for raw in [nil, "", "unknown=3", "claude=-1", "codex=x", "grok", "gemini=1.5"] {
            XCTAssertNil(DemoMode.parseForcedInFlight(raw), "\(raw ?? "nil") must not force counts")
        }
    }
}
