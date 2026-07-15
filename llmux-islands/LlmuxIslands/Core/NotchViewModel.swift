//
//  NotchViewModel.swift
//  ClaudeIsland
//
//  State management for the dynamic island
//

import AppKit
import Combine
import SwiftUI

enum NotchStatus: Equatable {
    case closed
    case opened
    case popping
}

enum NotchOpenReason {
    case click
    case hover
    case notification
    case usageAlert
    case boot
    case unknown

    var sharedCoreValue: String {
        switch self {
        case .click: return "click"
        case .hover: return "hover"
        case .notification: return "notification"
        case .usageAlert: return "usage_alert"
        case .boot: return "boot"
        case .unknown: return "click"
        }
    }
}

enum NotchContentType: Equatable {
    case usage
    case stats
    case menu

    var id: String {
        switch self {
        case .usage: return "usage"
        case .stats: return "stats"
        case .menu: return "menu"
        }
    }

    var sharedCoreValue: String {
        switch self {
        case .usage: return "usage"
        case .stats: return "statistics"
        case .menu: return "menu"
        }
    }
}

@MainActor
class NotchViewModel: ObservableObject {
    // MARK: - Published State

    @Published var status: NotchStatus = .closed
    @Published var openReason: NotchOpenReason = .unknown
    @Published var contentType: NotchContentType = .usage
    @Published var isHovering: Bool = false

    // MARK: - Dependencies

    private let screenSelector = ScreenSelector.shared
    private let soundSelector = SoundSelector.shared

    // MARK: - Geometry

    let geometry: NotchGeometry
    let spacing: CGFloat = 12
    let hasPhysicalNotch: Bool
    private let openedSizeOverride: CGSize?

    var deviceNotchRect: CGRect { geometry.deviceNotchRect }
    var screenRect: CGRect { geometry.screenRect }
    var windowHeight: CGFloat { geometry.windowHeight }

    /// Dynamic opened size based on content type
    var openedSize: CGSize {
        if let openedSizeOverride {
            return openedSizeOverride
        }
        switch contentType {
        case .usage:
            return CGSize(
                width: min(screenRect.width * 0.5, 560),
                height: usageOpenedHeight
            )
        case .stats:
            return CGSize(
                width: min(screenRect.width * 0.5, 560),
                height: max(440, min(480, screenRect.height - 72))
            )
        case .menu:
            let baseMenuHeight: CGFloat = 460
            let expandedHeight = screenSelector.expandedPickerHeight + soundSelector.expandedPickerHeight
            let maxMenuHeight = max(420, min(windowHeight - 24, screenRect.height - 72))
            return CGSize(
                width: min(screenRect.width * 0.4, 500),
                height: min(baseMenuHeight + expandedHeight, maxMenuHeight)
            )
        }
    }

    /// Height of the default compact account list. Advanced details remain in
    /// the same bounded scroll surface and do not alter semantic window state.
    private var usageOpenedHeight: CGFloat {
        let count = IslandUsageModel.shared.tiles.count
        let chrome: CGFloat = 154
        let perAccount: CGFloat = 48
        let desired = chrome + CGFloat(min(max(count, 1), 7)) * perAccount
        let minHeight: CGFloat = 340
        let maxHeight = max(minHeight, screenRect.height - 72)
        return min(max(desired, minHeight), min(580, maxHeight))
    }

    // MARK: - Animation

    var animation: Animation {
        .easeOut(duration: 0.25)
    }

    // MARK: - Private

    private var cancellables = Set<AnyCancellable>()
    private let events = EventMonitors.shared
    private var hoverTimer: DispatchWorkItem?

    // MARK: - Navigation State

    private var lastNonMenuContentType: NotchContentType = .usage

    // MARK: - Initialization

    init(
        deviceNotchRect: CGRect,
        screenRect: CGRect,
        windowHeight: CGFloat,
        hasPhysicalNotch: Bool,
        openedSizeOverride: CGSize? = nil
    ) {
        self.geometry = NotchGeometry(
            deviceNotchRect: deviceNotchRect,
            screenRect: screenRect,
            windowHeight: windowHeight
        )
        self.hasPhysicalNotch = hasPhysicalNotch
        self.openedSizeOverride = openedSizeOverride
        setupEventHandlers()
        observeSelectors()
    }

    private func observeSelectors() {
        screenSelector.$isPickerExpanded
            .sink { [weak self] _ in
                self?.objectWillChange.send()
                self?.reportWindowMetrics()
            }
            .store(in: &cancellables)

        soundSelector.$isPickerExpanded
            .sink { [weak self] _ in
                self?.objectWillChange.send()
                self?.reportWindowMetrics()
            }
            .store(in: &cancellables)

        // Re-evaluate `openedSize` whenever the account count changes so the
        // usage panel grows/shrinks with the number of tiles (see usageOpenedHeight).
        IslandUsageModel.shared.$tiles
            .map(\.count)
            .removeDuplicates()
            .sink { [weak self] _ in
                self?.objectWillChange.send()
                self?.reportWindowMetrics()
            }
            .store(in: &cancellables)
    }

    // MARK: - Event Handling

    private func setupEventHandlers() {
        events.mouseLocation
            .throttle(for: .milliseconds(50), scheduler: DispatchQueue.main, latest: true)
            .sink { [weak self] location in
                self?.handleMouseMove(location)
            }
            .store(in: &cancellables)

        events.mouseDown
            .receive(on: DispatchQueue.main)
            .sink { [weak self] _ in
                self?.handleMouseDown()
            }
            .store(in: &cancellables)
    }

    private func handleMouseMove(_ location: CGPoint) {
        let inNotch = geometry.isPointInNotch(location)
        let inOpened = status == .opened && geometry.isPointInOpenedPanel(location, size: openedSize)

        let newHovering = inNotch || inOpened

        // Only update if changed to prevent unnecessary re-renders
        guard newHovering != isHovering else { return }

        isHovering = newHovering

        // Cancel any pending hover timer
        hoverTimer?.cancel()
        hoverTimer = nil

        // Start hover timer to auto-expand after 1 second
        if isHovering && (status == .closed || status == .popping) {
            let workItem = DispatchWorkItem { [weak self] in
                guard let self = self, self.isHovering else { return }
                self.notchOpen(reason: .hover)
            }
            hoverTimer = workItem
            DispatchQueue.main.asyncAfter(deadline: .now() + 1.0, execute: workItem)
        }
    }

    private func handleMouseDown() {
        let location = NSEvent.mouseLocation

        switch status {
        case .opened:
            if geometry.isPointOutsidePanel(location, size: openedSize) {
                notchClose()
                // Re-post the click so it reaches the window/app behind us
                repostClickAt(location)
            } else if geometry.notchScreenRect.contains(location) {
                // Clicking the notch while opened closes the island.
                notchClose()
            }
        case .closed, .popping:
            if geometry.isPointInNotch(location) {
                notchOpen(reason: .click)
            }
        }
    }

    /// Re-posts a mouse click at the given screen location so it reaches windows behind us
    private func repostClickAt(_ location: CGPoint) {
        // Small delay to let the window's ignoresMouseEvents update
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.05) {
            // Convert to CGEvent coordinate system (screen coordinates with Y from top-left)
            guard let screen = NSScreen.main else { return }
            let screenHeight = screen.frame.height
            let cgPoint = CGPoint(x: location.x, y: screenHeight - location.y)

            // Create and post mouse down event
            if let mouseDown = CGEvent(
                mouseEventSource: nil,
                mouseType: .leftMouseDown,
                mouseCursorPosition: cgPoint,
                mouseButton: .left
            ) {
                mouseDown.post(tap: .cghidEventTap)
            }

            // Create and post mouse up event
            if let mouseUp = CGEvent(
                mouseEventSource: nil,
                mouseType: .leftMouseUp,
                mouseCursorPosition: cgPoint,
                mouseButton: .left
            ) {
                mouseUp.post(tap: .cghidEventTap)
            }
        }
    }

    // MARK: - Actions

    func notchOpen(reason: NotchOpenReason = .unknown) {
        openReason = reason
        status = .opened
        let size = openedSize
        Task {
            await IslandUsageModel.shared.nativeWindowOpened(
                reason: reason.sharedCoreValue,
                size: size
            )
        }
    }

    func notchClose() {
        // In demo/recording mode the island must stay open for the screen
        // recorder — swallow every close request.
        guard !DemoMode.isActive else { return }
        status = .closed
        lastNonMenuContentType = .usage
        contentType = .usage
        Task { await IslandUsageModel.shared.nativeWindowClosed() }
    }

    /// Open the island and keep it open for screen recording (demo mode). With
    /// `notchClose()` neutered above, this simply pins the usage panel visible.
    func enterDemoHold() {
        contentType = .usage
        notchOpen(reason: .boot)
    }

    func notchPop() {
        guard status == .closed else { return }
        status = .popping
    }

    func notchUnpop() {
        guard status == .popping else { return }
        status = .closed
    }

    func toggleMenu() {
        if contentType == .menu {
            contentType = lastNonMenuContentType == .menu ? .usage : lastNonMenuContentType
            reportNavigation()
            return
        }

        lastNonMenuContentType = contentType
        contentType = .menu
        reportNavigation()
    }

    func showUsage() {
        lastNonMenuContentType = .usage
        contentType = .usage
        reportNavigation()
    }

    /// Open the Statistics panel (issue #68 v2) — reached only through the
    /// ☰ menu's "Statistics" entry; the island always REOPENS on `.usage`
    /// (`notchClose()` resets the content type).
    func showStats() {
        lastNonMenuContentType = .stats
        contentType = .stats
        reportNavigation()
    }

    /// Open Settings directly from the compact top navigation. The semantic
    /// core retains its established `menu` route name; only the shell label is
    /// renewed.
    func showSettings() {
        guard contentType != .menu else { return }
        lastNonMenuContentType = contentType
        contentType = .menu
        reportNavigation()
    }

    private func reportNavigation() {
        let navigation = contentType.sharedCoreValue
        let size = openedSize
        Task {
            await IslandUsageModel.shared.nativeNavigationSelected(navigation, size: size)
        }
    }

    private func reportWindowMetrics() {
        let size = openedSize
        Task { await IslandUsageModel.shared.nativeWindowMetricsChanged(size: size) }
    }

    /// Perform boot animation: expand briefly then collapse
    func performBootAnimation() {
        notchOpen(reason: .boot)
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.0) { [weak self] in
            guard let self = self, self.openReason == .boot else { return }
            self.notchClose()
        }
    }
}
