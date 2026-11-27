//
//  NotchWindow.swift
//  ClaudeIsland
//
//  Transparent window that overlays the notch area
//  Following NotchDrop's approach: window ignores mouse events,
//  we use global event monitors to detect clicks/hovers
//

import AppKit

// Use NSPanel subclass for non-activating behavior
class NotchPanel: NSPanel {
    /// The island sits above the menu bar.
    static let islandLevel = NSWindow.Level(rawValue: NSWindow.Level.mainMenu.rawValue + 3)
    /// A window the island opens (folder picker) must sit above the island,
    /// or it opens behind the open panel and cannot be used.
    static let modalAboveIslandLevel = NSWindow.Level(rawValue: islandLevel.rawValue + 1)

    /// Panels the island opened that are still up (the folder picker).
    /// While one is, the island neither opens nor closes by itself.
    @MainActor static private(set) var openPanels = 0

    /// Shows `panel` above the island WITHOUT a modal session, then calls
    /// `completion` with the result. A modal session (`runModal()`) blocks
    /// every other window of the app while the picker is up — the island
    /// looked frozen, worst when the picker did not come to the front (owner
    /// 2026-11-27). Non-modal, the island keeps responding, and the level is
    /// set after the panel is ordered in, where it sticks (measured with
    /// CGWindowListCopyWindowInfo: picker 28, island 27).
    @MainActor
    static func beginAboveIsland(_ panel: NSSavePanel,
                                 completion: @escaping @MainActor (NSApplication.ModalResponse) -> Void) {
        openPanels += 1
        panel.begin { response in
            MainActor.assumeIsolated {
                openPanels -= 1
                completion(response)
            }
        }
        panel.level = modalAboveIslandLevel
        NSApp.activate()
        panel.makeKeyAndOrderFront(nil)
    }

    override init(
        contentRect: NSRect,
        styleMask style: NSWindow.StyleMask,
        backing backingStoreType: NSWindow.BackingStoreType,
        defer flag: Bool
    ) {
        super.init(
            contentRect: contentRect,
            styleMask: [.borderless, .nonactivatingPanel],
            backing: .buffered,
            defer: false
        )

        // Floating panel behavior
        isFloatingPanel = true
        becomesKeyOnlyIfNeeded = true

        // Transparent configuration
        isOpaque = false
        titleVisibility = .hidden
        titlebarAppearsTransparent = true
        backgroundColor = .clear
        hasShadow = false

        // CRITICAL: Prevent window from moving during space switches
        isMovable = false

        // Window behavior - stays on all spaces, above menu bar
        collectionBehavior = [
            .fullScreenAuxiliary,
            .stationary,
            .canJoinAllSpaces,
            .ignoresCycle
        ]

        // Above the menu bar
        level = Self.islandLevel

        // Enable tooltips even when app is inactive (needed for panel windows)
        allowsToolTipsWhenApplicationIsInactive = true

        // CRITICAL: Window ignores ALL mouse events
        // This allows clicks to pass through to the menu bar
        // We use global event monitors to detect hover/clicks on the notch area
        ignoresMouseEvents = true

        isReleasedWhenClosed = true
        acceptsMouseMovedEvents = false
    }

    override var canBecomeKey: Bool { true }
    override var canBecomeMain: Bool { false }

    // MARK: - Click-through for areas outside the panel content

    override func sendEvent(_ event: NSEvent) {
        // For mouse events, check if we should pass through
        if event.type == .leftMouseDown || event.type == .leftMouseUp ||
           event.type == .rightMouseDown || event.type == .rightMouseUp {
            // Get the location in window coordinates
            let locationInWindow = event.locationInWindow

            // Check if any view wants to handle this event
            if let contentView = self.contentView,
               contentView.hitTest(locationInWindow) == nil {
                // No view wants this event - pass it through to windows behind
                // by temporarily ignoring mouse events and re-posting
                let screenLocation = convertPoint(toScreen: locationInWindow)
                ignoresMouseEvents = true

                // Re-post the event after a tiny delay
                DispatchQueue.main.async { [weak self] in
                    self?.repostMouseEvent(event, at: screenLocation)
                }
                return
            }
        }

        super.sendEvent(event)
    }

    private func repostMouseEvent(_ event: NSEvent, at screenLocation: NSPoint) {
        // Convert to CGEvent coordinate system (Y from top of screen)
        guard let screen = NSScreen.main else { return }
        let screenHeight = screen.frame.height
        let cgPoint = CGPoint(x: screenLocation.x, y: screenHeight - screenLocation.y)

        let mouseType: CGEventType
        switch event.type {
        case .leftMouseDown: mouseType = .leftMouseDown
        case .leftMouseUp: mouseType = .leftMouseUp
        case .rightMouseDown: mouseType = .rightMouseDown
        case .rightMouseUp: mouseType = .rightMouseUp
        default: return
        }

        let mouseButton: CGMouseButton = event.type == .rightMouseDown || event.type == .rightMouseUp ? .right : .left

        if let cgEvent = CGEvent(
            mouseEventSource: nil,
            mouseType: mouseType,
            mouseCursorPosition: cgPoint,
            mouseButton: mouseButton
        ) {
            cgEvent.post(tap: .cghidEventTap)
        }
    }
}
