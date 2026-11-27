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
        // The bracket runs past the 7d fill, under empty track.
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

    func testBracketEndsOnTheEdgeOfFablesLastSegment() {
        // 10 segments with 1-point gaps on a 109-point bar: 10 points each.
        let fable = SevenDayReadout.remaining(usedPercent: 40) // 60% left
        XCTAssertEqual(SevenDayReadout.bracketEnd(fable, width: 109), 65) // 6 × 10 + 5 gaps
        XCTAssertEqual(SevenDayReadout.bracketEnd(1, width: 109), 109, "full Fable spans the bar")
        XCTAssertEqual(SevenDayReadout.bracketEnd(0.04, width: 109), 0, "nothing lit: start tick only")
        XCTAssertEqual(SevenDayReadout.bracketEnd(nil, width: 109), 0)
    }

    func testBracketEndDoesNotDependOnTheSevenDayFill() {
        // Same Fable value, any 7d value: the bracket is its own series.
        let fable = SevenDayReadout.remaining(usedPercent: 10) // 90% left
        XCTAssertEqual(SevenDayReadout.bracketEnd(fable, width: 109), 98)
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
