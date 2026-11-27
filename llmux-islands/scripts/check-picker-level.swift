// Checks on a real Mac that the folder picker opens ABOVE the island,
// through the production code (NotchPanel and NotchPanel.runModalAboveIsland):
//
//   swiftc -parse-as-library -O llmux-islands/scripts/check-picker-level.swift \
//       llmux-islands/LlmuxIslands/UI/Window/NotchWindow.swift -o /tmp/check-picker-level
//   /tmp/check-picker-level
//
// Shows a NotchPanel, opens an NSOpenPanel the way StartCodingView does,
// and reads the window server's layers while the picker is modal. Exits 1
// if the picker is not above the island. (A level set before runModal() is
// reset to 8 by the modal session; that is the bug this guards against.)
// Needs a logged-in desktop session, so it runs on a Mac, not in CI.
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
        let check = Timer(timeInterval: 1.0, repeats: false) { _ in
            MainActor.assumeIsolated {
                picker = layer(of: panel)
                islandLayer = layer(of: island)
                panel.cancel(nil)
            }
        }
        RunLoop.main.add(check, forMode: .common)
        _ = NotchPanel.runModalAboveIsland(panel)

        print("picker layer \(picker.map(String.init) ?? "?"), island layer \(islandLayer.map(String.init) ?? "?")")
        guard let picker, let islandLayer, picker > islandLayer else {
            print("FAIL: the picker is not above the island")
            exit(1)
        }
        print("ok: the picker is above the island")
    }
}
