import AppKit

/// Borderless floating panel hosting the island UI near the top of the screen.
/// Unlike agent-island's pass-through panel (which sets `ignoresMouseEvents` and
/// relies on global event monitors), this one accepts mouse events so the
/// SwiftUI controls inside are directly interactive. It is shown / hidden from
/// the menu-bar item.
final class NotchPanel: NSPanel {
    init(size: NSSize) {
        super.init(
            contentRect: NSRect(origin: .zero, size: size),
            styleMask: [.borderless, .nonactivatingPanel],
            backing: .buffered,
            defer: false
        )
        isFloatingPanel = true
        level = .mainMenu + 2
        isOpaque = false
        backgroundColor = .clear
        hasShadow = true
        isMovable = false
        hidesOnDeactivate = false
        collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .stationary]
    }

    override var canBecomeKey: Bool { true }
    override var canBecomeMain: Bool { false }
}
