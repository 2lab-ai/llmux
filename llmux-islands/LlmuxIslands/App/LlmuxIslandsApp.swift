import SwiftUI

/// Entry point. The real UI lives in a custom borderless panel created by
/// `AppDelegate`; the `Settings` scene is an empty placeholder (this is an
/// `LSUIElement` menu-bar/notch app with no standard window).
@main
struct LlmuxIslandsApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) var appDelegate

    var body: some Scene {
        Settings {
            EmptyView()
        }
    }
}
