//
//  WindowManager.swift
//  ClaudeIsland
//
//  Manages the notch window lifecycle
//

import AppKit
import os.log

/// Logger for window management
private let logger = Logger(subsystem: "com.claudeisland", category: "Window")

@MainActor
class WindowManager {
    private(set) var windowController: NotchWindowController?
    private var launched = false

    /// Set up or recreate the notch window
    func setupNotchWindow() -> NotchWindowController? {
        // Use ScreenSelector for screen selection
        let screenSelector = ScreenSelector.shared
        screenSelector.refreshScreens()

        guard let screen = screenSelector.selectedScreen else {
            logger.warning("No screen found")
            return nil
        }

        if let existingController = windowController {
            // Display changes fire often (remote screen resize, display
            // wake, menu bar changes). Keep the window — and whatever the
            // owner has open in it — when its screen geometry is unchanged.
            let signature = NotchWindowController.ScreenSignature(
                frame: screen.frame, notchSize: screen.notchSize, hasPhysicalNotch: screen.hasPhysicalNotch
            )
            if existingController.builtFor == signature {
                return existingController
            }
            existingController.window?.orderOut(nil)
            existingController.window?.close()
            windowController = nil
        }

        windowController = NotchWindowController(screen: screen, playBootAnimation: !launched)
        launched = true
        windowController?.showWindow(nil)

        return windowController
    }
}
