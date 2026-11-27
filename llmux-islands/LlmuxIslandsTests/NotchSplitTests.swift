import XCTest

/// Which closed-label items go left and right of a physical notch: the
/// split whose wider wing is narrowest, so neither wing is an empty bar.
final class NotchSplitTests: XCTestCase {
    // Rough widths in points: mascot 17.8, a provider group 22.7, chip
    // "AUTH 3" 50, quiet dot 5.
    private let mascot: CGFloat = 17.8
    private let group: CGFloat = 22.7
    private let chip: CGFloat = 50
    private let dot: CGFloat = 5

    func testOneGroupAndAChipPutTheGroupBesideTheMascot() {
        // Owner screenshot 2026-11-27: crab | ✳2 AUTH 3 left the left wing
        // a long empty bar. Balanced: crab ✳2 | AUTH 3.
        XCTAssertEqual(NotchSplit.best(widths: [mascot, group, chip], spacing: 8), 2)
    }

    func testIdleKeepsTheMascotLeftAndTheDotRight() {
        XCTAssertEqual(NotchSplit.best(widths: [mascot, dot], spacing: 8), 1)
    }

    func testAChipAloneStaysRight() {
        XCTAssertEqual(NotchSplit.best(widths: [mascot, chip], spacing: 8), 1)
    }

    func testManyGroupsSplitEvenly() {
        let left = NotchSplit.best(widths: [mascot, group, group, group, group], spacing: 8)
        XCTAssertEqual(left, 3, "crab ✳ ◎ | G ✦")
    }

    func testTheWingsComeOutClose() {
        let widths: [CGFloat] = [mascot, group, chip]
        let left = NotchSplit.best(widths: widths, spacing: 8)
        let l = NotchSplit.width(widths[..<left], spacing: 8)
        let r = NotchSplit.width(widths[left...], spacing: 8)
        XCTAssertLessThan(abs(l - r), chip, "never worse than moving one item")
        XCTAssertEqual(NotchSplit.width([CGFloat](), spacing: 8), 0)
        XCTAssertEqual(NotchSplit.width([mascot], spacing: 8), mascot)
    }

    func testASingleItemGoesLeft() {
        XCTAssertEqual(NotchSplit.best(widths: [mascot], spacing: 8), 1)
        XCTAssertEqual(NotchSplit.best(widths: [], spacing: 8), 0)
    }
}
