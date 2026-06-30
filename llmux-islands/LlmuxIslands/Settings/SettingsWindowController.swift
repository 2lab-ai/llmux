import AppKit
import SwiftUI

/// Hosts the standalone Settings window (`SettingsView`). Because this is an
/// `LSUIElement` accessory app, the activation policy is bumped to `.regular`
/// while the window is open (so it can take focus and appear in the switcher)
/// and dropped back to `.accessory` when it closes.
final class SettingsWindowController: NSWindowController, NSWindowDelegate {
    convenience init(model: IslandUsageModel) {
        let hosting = NSHostingController(rootView: SettingsView(model: model))
        let window = NSWindow(contentViewController: hosting)
        window.title = "llmux-islands Settings"
        window.styleMask = [.titled, .closable, .miniaturizable]
        window.isReleasedWhenClosed = false
        window.setContentSize(NSSize(width: 460, height: 540))
        window.center()
        self.init(window: window)
        window.delegate = self
    }

    func show() {
        NSApp.setActivationPolicy(.regular)
        window?.makeKeyAndOrderFront(nil)
        window?.center()
        NSApp.activate(ignoringOtherApps: true)
    }

    func windowWillClose(_ notification: Notification) {
        NSApp.setActivationPolicy(.accessory)
    }
}
