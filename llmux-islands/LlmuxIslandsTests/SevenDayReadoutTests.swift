import XCTest

/// The tile's 7d row with Fable folded in: `60% (40%)`, one bar.
final class SevenDayReadoutTests: XCTestCase {
    func testOwnerExampleSixtyFortyRendersAsSixAndFourSegments() {
        let seven = SevenDayReadout.remaining(usedPercent: 40)
        let fable = SevenDayReadout.remaining(usedPercent: 60)
        XCTAssertEqual(SevenDayReadout.percentText(seven) + SevenDayReadout.fableSuffix(fable), "60% (40%)")
        XCTAssertEqual(SevenDayReadout.litSegments(seven), 6)
        XCTAssertEqual(SevenDayReadout.litSegments(fable), 4)
    }

    func testFableWithMoreLeftThanSevenDayStillShows() {
        // Fable's bar behind runs past the 7d fill.
        let seven = SevenDayReadout.remaining(usedPercent: 80)
        let fable = SevenDayReadout.remaining(usedPercent: 10)
        XCTAssertEqual(SevenDayReadout.litSegments(seven), 2)
        XCTAssertEqual(SevenDayReadout.litSegments(fable), 9)
        XCTAssertEqual(SevenDayReadout.percentText(seven) + SevenDayReadout.fableSuffix(fable), "20% (90%)")
    }

    func testNoFableMeansNoBracket() {
        XCTAssertEqual(SevenDayReadout.fableSuffix(nil), "")
        XCTAssertEqual(SevenDayReadout.litSegments(nil), 0)
    }

    func testUnreportedAndOutOfRangeValues() {
        XCTAssertNil(SevenDayReadout.remaining(usedPercent: nil))
        XCTAssertNil(SevenDayReadout.remaining(usedPercent: .nan))
        XCTAssertEqual(SevenDayReadout.percentText(nil), "--")
        XCTAssertEqual(SevenDayReadout.remaining(usedPercent: 130), 0)
        XCTAssertEqual(SevenDayReadout.remaining(usedPercent: -5), 1)
        XCTAssertEqual(SevenDayReadout.litSegments(0.05), 1, "rounds to the nearest segment")
        XCTAssertEqual(SevenDayReadout.litSegments(0.04), 0)
    }
}
