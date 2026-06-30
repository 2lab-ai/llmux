import AppKit
import Darwin
import SwiftUI

/// Owns the floating-island window and starts the accounts model. Stripped of
/// agent-island's Sparkle / Mixpanel / session-monitor / hook machinery — only
/// the island shell remains, driven by the llmux HTTP API.
@MainActor
class AppDelegate: NSObject, NSApplicationDelegate {
    static var shared: AppDelegate?

    private var windowManager: WindowManager?
    private var screenObserver: ScreenObserver?
    private var settingsController: SettingsWindowController?

    var windowController: NotchWindowController? {
        windowManager?.windowController
    }

    /// Open (or focus) the standalone Settings window.
    @MainActor
    func openSettings() {
        if settingsController == nil {
            settingsController = SettingsWindowController(model: IslandUsageModel.shared)
        }
        settingsController?.show()
    }

    override init() {
        super.init()
        AppDelegate.shared = self
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApplication.shared.setActivationPolicy(.accessory)

        windowManager = WindowManager()
        _ = windowManager?.setupNotchWindow()

        screenObserver = ScreenObserver { [weak self] in
            _ = self?.windowManager?.setupNotchWindow()
        }

        Task { @MainActor in
            IslandUsageModel.shared.start()
        }

        // Launch with `--open-settings` to open the Settings window directly
        // (useful when there is no notch to hover, and for verification).
        if CommandLine.arguments.contains("--open-settings") {
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.3) { [weak self] in
                self?.openSettings()
            }
        }
    }

    func applicationWillTerminate(_ notification: Notification) {
        screenObserver = nil
    }

    @MainActor
    func requestTerminateFromMenu() {
        NSApplication.shared.terminate(nil)
        // Some non-activating panel states can swallow the regular terminate
        // flow; keep a short fallback so "Quit" always exits.
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) {
            if NSApplication.shared.isRunning {
                Darwin.exit(0)
            }
        }
    }
}
