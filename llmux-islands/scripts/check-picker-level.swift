// Checks on a real Mac that the folder picker opens ABOVE the island.
//
//   swiftc -O llmux-islands/scripts/check-picker-level.swift -o /tmp/check-picker-level
//   /tmp/check-picker-level
//
// Opens an island-level panel (mainMenu + 3) and an NSOpenPanel the way
// StartCodingView does, then reads the window server's layers while the
// picker is modal. Exits 1 if the picker is not above the island. A level
// set before runModal() is reset to 8 by the modal session; that is the bug
// this guards against. Needs a logged-in desktop session (not CI).
import AppKit

setvbuf(stdout, nil, _IONBF, 0)
let app = NSApplication.shared
app.setActivationPolicy(.accessory)
let islandLevel = NSWindow.Level(rawValue: NSWindow.Level.mainMenu.rawValue + 3)
let aboveIsland = NSWindow.Level(rawValue: islandLevel.rawValue + 1)
let island = NSPanel(contentRect: NSRect(x: 0, y: 200, width: 600, height: 300),
                     styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: false)
island.level = islandLevel
island.backgroundColor = NSColor.black.withAlphaComponent(0.01)
island.orderFrontRegardless()

func layer(of window: NSWindow) -> Int? {
    let list = CGWindowListCopyWindowInfo([.optionIncludingWindow], CGWindowID(window.windowNumber)) as? [[String: Any]]
    return list?.first?[kCGWindowLayer as String] as? Int
}

// Same steps as NotchPanel.runModalAboveIsland.
let panel = NSOpenPanel()
panel.canChooseDirectories = true
var result: (picker: Int?, island: Int?) = (nil, nil)
let raise = Timer(timeInterval: 0, repeats: false) { _ in panel.level = aboveIsland }
RunLoop.main.add(raise, forMode: .modalPanel)
let check = Timer(timeInterval: 1.0, repeats: false) { _ in
    result = (layer(of: panel), layer(of: island))
    panel.cancel(nil)
}
RunLoop.main.add(check, forMode: .common)
NSApp.activate(ignoringOtherApps: true)
_ = panel.runModal()

print("picker layer \(result.picker.map(String.init) ?? "?"), island layer \(result.island.map(String.init) ?? "?")")
guard let picker = result.picker, let islandLayer = result.island, picker > islandLayer else {
    print("FAIL: the picker is not above the island")
    exit(1)
}
print("ok: the picker is above the island")
