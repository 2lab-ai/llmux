//
//  ScreenObserver.swift
//  ClaudeIsland
//
//  Monitors screen configuration changes
//

import AppKit

@MainActor
class ScreenObserver {
    private var observer: Any?
    private let onScreenChange: () -> Void

    init(onScreenChange: @escaping () -> Void) {
        self.onScreenChange = onScreenChange
        startObserving()
    }

    deinit {
        // deinit is nonisolated; remove the observer inline rather than calling
        // the @MainActor stopObserving().
        if let observer {
            NotificationCenter.default.removeObserver(observer)
        }
    }

    private func startObserving() {
        observer = NotificationCenter.default.addObserver(
            forName: NSApplication.didChangeScreenParametersNotification,
            object: nil,
            queue: .main
        ) { [weak self] _ in
            self?.onScreenChange()
        }
    }

}
