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
}

enum NotchContentType: Equatable {
    case usage
    case menu

    var id: String {
        switch self {
        case .usage: return "usage"
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

    var deviceNotchRect: CGRect { geometry.deviceNotchRect }
    var screenRect: CGRect { geometry.screenRect }
    var windowHeight: CGFloat { geometry.windowHeight }

    /// Dynamic opened size based on content type
    var openedSize: CGSize {
        switch contentType {
        case .usage:
            return CGSize(
                width: min(screenRect.width * 0.5, 600),
                height: usageOpenedHeight
            )
        case .menu:
            // Menu has many fixed-height rows; 420 can push bottom actions outside
            // the interactive panel hit area. Keep a larger base height.
            let baseMenuHeight: CGFloat = 520
            let expandedHeight = screenSelector.expandedPickerHeight + soundSelector.expandedPickerHeight
            let maxMenuHeight = max(420, min(windowHeight - 24, screenRect.height - 72))
            return CGSize(
                width: min(screenRect.width * 0.4, 480),
                height: min(baseMenuHeight + expandedHeight, maxMenuHeight)
            )
        }
    }

    /// Height of the usage panel, sized to the number of account tiles.
    /// The grid is two columns, so rows = ceil(count / 2); 1–2 accounts fit a
    /// single row (no tall empty void), 3–4 take two rows, 5+ keep growing and
    /// then scroll once they hit the screen limit — instead of a fixed 640 that
    /// left most of the panel black. `perRow`/`chrome` are tuned to the rendered
    /// tile height (token footer removed, usage rows enlarged).
    private var usageOpenedHeight: CGFloat {
        let count = IslandUsageModel.shared.tiles.count
        let chrome: CGFloat = 96          // notch header + "Usage" toolbar + paddings
        let perRow: CGFloat = 186         // one grid row of enlarged tiles (measured ≈180)
        let rowSpacing: CGFloat = 10
        let rows = max(1, Int(ceil(Double(max(count, 1)) / 2.0)))
        let desired = chrome + CGFloat(rows) * perRow + CGFloat(max(0, rows - 1)) * rowSpacing
        let minHeight: CGFloat = 240
        let maxHeight = max(minHeight, screenRect.height - 72)
        return min(max(desired, minHeight), maxHeight)
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

    init(deviceNotchRect: CGRect, screenRect: CGRect, windowHeight: CGFloat, hasPhysicalNotch: Bool) {
        self.geometry = NotchGeometry(
            deviceNotchRect: deviceNotchRect,
            screenRect: screenRect,
            windowHeight: windowHeight
        )
        self.hasPhysicalNotch = hasPhysicalNotch
        setupEventHandlers()
        observeSelectors()
    }

    private func observeSelectors() {
        screenSelector.$isPickerExpanded
            .sink { [weak self] _ in self?.objectWillChange.send() }
            .store(in: &cancellables)

        soundSelector.$isPickerExpanded
            .sink { [weak self] _ in self?.objectWillChange.send() }
            .store(in: &cancellables)

        // Re-evaluate `openedSize` whenever the account count changes so the
        // usage panel grows/shrinks with the number of tiles (see usageOpenedHeight).
        IslandUsageModel.shared.$tiles
            .map(\.count)
            .removeDuplicates()
            .sink { [weak self] _ in self?.objectWillChange.send() }
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
    }

    func notchClose() {
        // In demo/recording mode the island must stay open for the screen
        // recorder — swallow every close request.
        guard !DemoMode.isActive else { return }
        status = .closed
        lastNonMenuContentType = .usage
        contentType = .usage
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
            return
        }

        lastNonMenuContentType = contentType
        contentType = .menu
    }

    func showUsage() {
        lastNonMenuContentType = .usage
        contentType = .usage
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
