import Foundation

/// Rules 1 and 4 of "When the island opens and closes" (docs/llmux-islands.md)
/// as a value type with an explicit clock, so their timing is testable:
///
/// - resting the pointer on the notch for `hoverOpenDelay` opens the island;
/// - an island that hover opened (and was not pinned by a click inside or the
///   folder picker) closes once the pointer has been away for
///   `leaveCloseDelay`;
/// - any pointer change cancels what was pending, so re-entering before the
///   deadline keeps the island open and a quick pass over the notch does not
///   open it;
/// - nothing is scheduled while a modal window (the folder picker) is up, and
///   when a deadline arrives the island is checked again as it is then
///   (pinned, busy or typing in a field: no close).
///
/// NotchViewModel feeds it pointer changes and calls `due` at `deadline`.
/// Foundation-only: the logic test target exercises it directly.
struct IslandHoverMachine: Equatable {
    /// Pointer dwell on the notch before the island opens by itself
    /// (boring.notch / Atoll use 0.3 s; Claude Island used 1 s).
    static let hoverOpenDelay: TimeInterval = 0.3
    /// Grace period after the pointer leaves a hover-opened island
    /// (boring.notch / Atoll 0.1 s; a little longer here so a big panel does
    /// not close when the pointer slips off an edge).
    static let leaveCloseDelay: TimeInterval = 0.5

    enum Action: Equatable {
        case open
        case close
    }

    /// The island as the machine needs to see it at each step.
    struct Island: Equatable {
        var isOpen: Bool
        /// Opened by hover and not pinned since.
        var openedByHover: Bool
        /// A modal window of the app is up (the folder picker).
        var busy: Bool = false
        /// The owner is typing in a field of the island.
        var editing: Bool = false
    }

    private(set) var hovering = false
    private(set) var pending: Action?
    private(set) var deadline: Date?

    /// The pointer entered or left the notch or the opened island.
    mutating func pointer(hovering newValue: Bool, island: Island, now: Date) {
        guard newValue != hovering else { return }
        hovering = newValue
        pending = nil
        deadline = nil
        guard !island.busy else { return }
        if hovering, !island.isOpen {
            schedule(.open, after: Self.hoverOpenDelay, now: now)
        } else if !hovering, island.isOpen, island.openedByHover {
            schedule(.close, after: Self.leaveCloseDelay, now: now)
        }
    }

    /// Called at `deadline`: what to do, checked against the island as it is
    /// now. nil when nothing is due or the reason for it has gone.
    mutating func due(island: Island, now: Date) -> Action? {
        guard let pending, let deadline, now >= deadline else { return nil }
        self.pending = nil
        self.deadline = nil
        switch pending {
        case .open:
            return hovering && !island.isOpen && !island.busy ? .open : nil
        case .close:
            let stillLeaving = !hovering && island.isOpen && island.openedByHover
            return stillLeaving && !island.busy && !island.editing ? .close : nil
        }
    }

    private mutating func schedule(_ action: Action, after delay: TimeInterval, now: Date) {
        pending = action
        deadline = now.addingTimeInterval(delay)
    }
}
