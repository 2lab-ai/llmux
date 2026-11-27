// Checks on a real Mac that the folder picker opens ABOVE the island and
// does NOT freeze the app, through the production code (NotchPanel and
// NotchPanel.beginAboveIsland):
//
//   swiftc -parse-as-library -O llmux-islands/scripts/check-picker-level.swift \
//       llmux-islands/LlmuxIslands/UI/Window/NotchWindow.swift -o /tmp/check-picker-level
//   /tmp/check-picker-level
//
// Shows a NotchPanel, opens an NSOpenPanel the way StartCodingView does, and
// while the picker is up reads the window server's layers and checks that an
// ordinary (default run loop mode) timer still fires — it does not inside a
// modal session, which is what made the island look frozen. Exits 1 on
// either failure. Needs a logged-in desktop session, so it runs on a Mac,
// not in CI.
import AppKit

@main
enum CheckPickerLevel {
    static func layer(of window: NSWindow) -> Int? {
        let info = CGWindowListCopyWindowInfo([.optionIncludingWindow], CGWindowID(window.windowNumber))
        return (info as? [[String: Any]])?.first?[kCGWindowLayer as String] as? Int
    }

    @MainActor
    static func main() {
        setvbuf(stdout, nil, _IONBF, 0)
        NSApplication.shared.setActivationPolicy(.accessory)
        let island = NotchPanel(contentRect: NSRect(x: 0, y: 200, width: 600, height: 300),
                                styleMask: [], backing: .buffered, defer: false)
        island.backgroundColor = NSColor.black.withAlphaComponent(0.01)
        island.orderFrontRegardless()

        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        var picker: Int?
        var islandLayer: Int?
        var responsive = false
        var done = false
        NotchPanel.beginAboveIsland(panel) { _ in done = true }
        // A default-mode timer: it never fires inside a modal session.
        Timer.scheduledTimer(withTimeInterval: 1.0, repeats: false) { _ in
            MainActor.assumeIsolated {
                responsive = true
                picker = layer(of: panel)
                islandLayer = layer(of: island)
                panel.cancel(nil)
            }
        }
        let deadline = Date().addingTimeInterval(10)
        while !done, Date() < deadline {
            RunLoop.main.run(mode: .default, before: Date().addingTimeInterval(0.1))
        }

        print("picker layer \(picker.map(String.init) ?? "?"), island layer \(islandLayer.map(String.init) ?? "?"), app responsive while picker up: \(responsive)")
        guard responsive else {
            print("FAIL: the app froze while the picker was up (modal session)")
            exit(1)
        }
        guard let picker, let islandLayer, picker > islandLayer else {
            print("FAIL: the picker is not above the island")
            exit(1)
        }
        print("ok: the picker is above the island and the app keeps responding")
    }
}
