import XCTest

/// Hover open and leave-close timing of the island (docs/llmux-islands.md,
/// "When the island opens and closes", rules 1 and 4), on a fake clock.
final class IslandHoverMachineTests: XCTestCase {
    private let t0 = Date(timeIntervalSinceReferenceDate: 1_000_000)
    private func at(_ seconds: TimeInterval) -> Date { t0.addingTimeInterval(seconds) }

    private let closed = IslandHoverMachine.Island(isOpen: false, openedByHover: false)
    private let hoverOpened = IslandHoverMachine.Island(isOpen: true, openedByHover: true)
    private let pinned = IslandHoverMachine.Island(isOpen: true, openedByHover: false)

    func testRestingOnTheNotchOpensAfterTheDwell() {
        var machine = IslandHoverMachine()
        machine.pointer(hovering: true, island: closed, now: at(0))
        XCTAssertEqual(machine.deadline, at(IslandHoverMachine.hoverOpenDelay))
        XCTAssertNil(machine.due(island: closed, now: at(0.1)), "not before the dwell")
        XCTAssertEqual(machine.due(island: closed, now: at(0.3)), .open)
    }

    func testAQuickPassOverTheNotchDoesNotOpen() {
        var machine = IslandHoverMachine()
        machine.pointer(hovering: true, island: closed, now: at(0))
        machine.pointer(hovering: false, island: closed, now: at(0.1))
        XCTAssertNil(machine.deadline)
        XCTAssertNil(machine.due(island: closed, now: at(0.3)))
    }

    func testAHoverOpenedIslandClosesAfterTheGracePeriod() {
        var machine = IslandHoverMachine()
        machine.pointer(hovering: true, island: closed, now: at(0))
        _ = machine.due(island: closed, now: at(0.3))
        machine.pointer(hovering: false, island: hoverOpened, now: at(2))
        XCTAssertNil(machine.due(island: hoverOpened, now: at(2.4)), "still in the grace period")
        XCTAssertEqual(machine.due(island: hoverOpened, now: at(2.5)), .close)
    }

    func testComingBackBeforeTheDeadlineKeepsItOpen() {
        var machine = IslandHoverMachine()
        machine.pointer(hovering: true, island: closed, now: at(0))
        _ = machine.due(island: closed, now: at(0.3))
        machine.pointer(hovering: false, island: hoverOpened, now: at(2))
        machine.pointer(hovering: true, island: hoverOpened, now: at(2.3))
        XCTAssertNil(machine.deadline, "re-entering cancels the close")
        XCTAssertNil(machine.due(island: hoverOpened, now: at(2.5)))
        XCTAssertNil(machine.due(island: hoverOpened, now: at(10)))
    }

    func testAClickInsideBeforeTheDeadlineKeepsItOpenPastIt() {
        var machine = IslandHoverMachine()
        machine.pointer(hovering: true, island: closed, now: at(0))
        _ = machine.due(island: closed, now: at(0.3))
        machine.pointer(hovering: false, island: hoverOpened, now: at(2))
        // The click pins the island (NotchViewModel.pin) before the deadline
        // fires; the deadline sees a pinned island and does not close it.
        XCTAssertNil(machine.due(island: pinned, now: at(2.5)))
    }

    func testAPinnedIslandNeverSchedulesAClose() {
        var machine = IslandHoverMachine()
        machine.pointer(hovering: true, island: pinned, now: at(0))
        machine.pointer(hovering: false, island: pinned, now: at(1))
        XCTAssertNil(machine.deadline)
    }

    func testNothingIsScheduledWhileThePickerIsUp() {
        var busy = hoverOpened
        busy.busy = true
        var machine = IslandHoverMachine()
        machine.pointer(hovering: true, island: closed, now: at(0))
        _ = machine.due(island: closed, now: at(0.3))
        machine.pointer(hovering: false, island: busy, now: at(1))
        XCTAssertNil(machine.deadline)
    }

    func testThePickerOrTypingAtTheDeadlineCancelsTheClose() {
        for change in [\IslandHoverMachine.Island.busy, \IslandHoverMachine.Island.editing] {
            var machine = IslandHoverMachine()
            machine.pointer(hovering: true, island: closed, now: at(0))
            _ = machine.due(island: closed, now: at(0.3))
            machine.pointer(hovering: false, island: hoverOpened, now: at(1))
            var later = hoverOpened
            later[keyPath: change] = true
            XCTAssertNil(machine.due(island: later, now: at(1.5)), "\(change)")
        }
    }

    func testTheOpenIsDroppedIfTheIslandOpenedMeanwhile() {
        // A click opened the island during the dwell: no second open.
        var machine = IslandHoverMachine()
        machine.pointer(hovering: true, island: closed, now: at(0))
        XCTAssertNil(machine.due(island: pinned, now: at(0.3)))
    }
}
