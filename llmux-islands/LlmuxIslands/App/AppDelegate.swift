import AppKit
import SwiftUI

/// Owns the menu-bar item and the borderless island panel, and starts the
/// accounts poller. Opening is via the menu-bar item (the panel renders as a
/// notch-styled island pinned to the top-center of the built-in display).
@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    private let model = AccountsViewModel()
    private var statusItem: NSStatusItem?
    private var panel: NotchPanel?

    private let panelSize = NSSize(width: 560, height: 480)

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.accessory)
        setupStatusItem()
        setupPanel()
        model.start()
    }

    private func setupStatusItem() {
        let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        if let button = item.button {
            button.image = NSImage(
                systemSymbolName: "gauge.with.dots.needle.67percent",
                accessibilityDescription: "llmux islands"
            )
            button.image?.isTemplate = true
            button.action = #selector(togglePanel)
            button.target = self
        }
        statusItem = item
    }

    private func setupPanel() {
        let root = RootView(onClose: { [weak self] in self?.hidePanel() })
            .environmentObject(model)
        let panel = NotchPanel(size: panelSize)
        panel.contentView = NSHostingView(rootView: root)
        self.panel = panel
    }

    private func positionPanel() {
        guard let panel, let screen = NSScreen.builtin ?? NSScreen.main else { return }
        let frame = screen.frame
        panel.setFrameOrigin(NSPoint(
            x: frame.midX - panelSize.width / 2,
            y: frame.maxY - panelSize.height
        ))
    }

    @objc private func togglePanel() {
        guard let panel else { return }
        if panel.isVisible {
            hidePanel()
        } else {
            positionPanel()
            panel.orderFrontRegardless()
            panel.makeKey()
            NSApp.activate(ignoringOtherApps: true)
            Task { await model.refresh() }
        }
    }

    private func hidePanel() {
        panel?.orderOut(nil)
    }
}
