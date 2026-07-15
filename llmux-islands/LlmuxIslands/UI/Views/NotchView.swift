//
//  NotchView.swift — the floating "dynamic island" root view.
//
//  Lifted verbatim from agent-island and stripped of the Claude-Code session
//  monitor / Sparkle update machinery: the notch shape, expand/collapse spring
//  animation, hover, and visibility behaviour are unchanged; the expanded panel
//  shows the llmux usage view or the settings menu.
//

import AppKit
import CoreGraphics
import SwiftUI

// Corner radius constants
private let cornerRadiusInsets = (
    opened: (top: CGFloat(19), bottom: CGFloat(24)),
    closed: (top: CGFloat(6), bottom: CGFloat(14))
)

struct NotchView: View {
    @ObservedObject var viewModel: NotchViewModel
    @StateObject private var activityCoordinator = NotchActivityCoordinator.shared
    @ObservedObject private var usageModel = IslandUsageModel.shared
    @State private var isVisible: Bool = false
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    private let snapshotAdvancedInitiallyPresented: Bool
    private let snapshotStatisticsPage: StatisticsAdvancedPage
    private let snapshotNow: Date?
    private let forceVisible: Bool

    @Namespace private var activityNamespace

    init(
        viewModel: NotchViewModel,
        snapshotAdvancedInitiallyPresented: Bool = false,
        snapshotStatisticsPage: StatisticsAdvancedPage = .analytics,
        snapshotNow: Date? = nil,
        forceVisible: Bool = false
    ) {
        self.viewModel = viewModel
        self.snapshotAdvancedInitiallyPresented = snapshotAdvancedInitiallyPresented
        self.snapshotStatisticsPage = snapshotStatisticsPage
        self.snapshotNow = snapshotNow
        self.forceVisible = forceVisible
    }

    // llmux-islands has no Claude-session monitor, so the closed-state "activity"
    // pill never lights up. These stay constant so the layout math is identical.
    private var hasPendingPermission: Bool { false }
    private var hasWaitingForInput: Bool { false }

    // MARK: - Sizing

    private var closedNotchSize: CGSize {
        CGSize(width: viewModel.deviceNotchRect.width, height: viewModel.deviceNotchRect.height)
    }

    private var expansionWidth: CGFloat {
        if activityCoordinator.expandingActivity.show {
            switch activityCoordinator.expandingActivity.type {
            case .claude:
                return 2 * max(0, closedNotchSize.height - 12) + 20
            case .none:
                break
            }
        }
        return 0
    }

    private var notchSize: CGSize {
        switch viewModel.status {
        case .closed, .popping:
            return closedNotchSize
        case .opened:
            return viewModel.openedSize
        }
    }

    private var closedContentWidth: CGFloat {
        closedNotchSize.width + expansionWidth
    }

    // MARK: - Corner Radii

    private var topCornerRadius: CGFloat {
        viewModel.status == .opened ? cornerRadiusInsets.opened.top : cornerRadiusInsets.closed.top
    }

    private var bottomCornerRadius: CGFloat {
        viewModel.status == .opened ? cornerRadiusInsets.opened.bottom : cornerRadiusInsets.closed.bottom
    }

    private var currentNotchShape: NotchShape {
        NotchShape(topCornerRadius: topCornerRadius, bottomCornerRadius: bottomCornerRadius)
    }

    private var panelAnimation: Animation? {
        reduceMotion ? nil : .spring(response: 0.28, dampingFraction: 1, blendDuration: 0)
    }

    // MARK: - Body

    var body: some View {
        ZStack(alignment: .top) {
            VStack(spacing: 0) {
                notchLayout
                    .frame(maxWidth: viewModel.status == .opened ? notchSize.width : nil, alignment: .top)
                    .padding(
                        .horizontal,
                        viewModel.status == .opened ? cornerRadiusInsets.opened.top : cornerRadiusInsets.closed.bottom
                    )
                    .padding([.horizontal, .bottom], viewModel.status == .opened ? 12 : 0)
                    .background(.black)
                    .clipShape(currentNotchShape)
                    .overlay(alignment: .top) {
                        Rectangle()
                            .fill(.black)
                            .frame(height: 1)
                            .padding(.horizontal, topCornerRadius)
                    }
                    .frame(
                        maxWidth: viewModel.status == .opened ? notchSize.width : nil,
                        maxHeight: viewModel.status == .opened ? notchSize.height : nil,
                        alignment: .top
                    )
                    .animation(panelAnimation, value: viewModel.status)
                    .contentShape(Rectangle())
                    .onTapGesture {
                        if viewModel.status != .opened {
                            viewModel.notchOpen(reason: .click)
                        }
                    }
            }
        }
        .opacity(forceVisible || isVisible ? 1 : 0)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .preferredColorScheme(.dark)
        .onAppear {
            // On non-notched displays keep the island visible so there's an
            // interaction target; on a real notch it stays hidden until hovered.
            if !viewModel.hasPhysicalNotch {
                isVisible = true
            }
        }
        .onChange(of: viewModel.status) { oldStatus, newStatus in
            handleStatusChange(from: oldStatus, to: newStatus)
        }
    }

    private var isProcessing: Bool {
        activityCoordinator.expandingActivity.show && activityCoordinator.expandingActivity.type == .claude
    }

    private var showClosedActivity: Bool {
        isProcessing || hasPendingPermission || hasWaitingForInput
    }

    // MARK: - Notch Layout

    @ViewBuilder
    private var notchLayout: some View {
        VStack(alignment: .leading, spacing: 0) {
            headerRow
                .frame(height: max(24, closedNotchSize.height))

            if viewModel.status == .opened {
                contentView
                    .frame(width: notchSize.width - 24)
            }
        }
    }

    @ViewBuilder
    private var headerRow: some View {
        HStack(spacing: 0) {
            if showClosedActivity {
                HStack(spacing: 4) {
                    ClaudeCrabIcon(size: 14, animateLegs: isProcessing)
                        .matchedGeometryEffect(id: "crab", in: activityNamespace, isSource: showClosedActivity)
                }
                .frame(width: viewModel.status == .opened ? nil : sideWidth)
                .padding(.leading, viewModel.status == .opened ? 8 : 0)
            }

            if viewModel.status == .opened {
                openedHeaderContent
            } else if !showClosedActivity {
                // Closed island: render the info label instead of a black box
                // (todo.md items 1–2). minWidth keeps the pill at least as wide
                // as the notch; wider content grows the pill to fit.
                NotchClosedLabelView(
                    claudeCount: usageModel.claudeInFlight,
                    codexCount: usageModel.codexInFlight,
                    grokCount: usageModel.grokInFlight,
                    signal: usageModel.glance,
                    active: isVisible
                )
                .frame(minWidth: closedNotchSize.width - 20)
            } else {
                Rectangle()
                    .fill(.black)
                    .frame(width: closedNotchSize.width - cornerRadiusInsets.closed.top)
            }

            if showClosedActivity, isProcessing {
                ProcessingSpinner()
                    .matchedGeometryEffect(id: "spinner", in: activityNamespace, isSource: showClosedActivity)
                    .frame(width: viewModel.status == .opened ? 20 : sideWidth)
            }
        }
        .frame(height: closedNotchSize.height)
    }

    private var sideWidth: CGFloat {
        max(0, closedNotchSize.height - 12) + 10
    }

    @ViewBuilder
    private var openedHeaderContent: some View {
        HStack(spacing: 10) {
            if !showClosedActivity {
                ClaudeCrabIcon(size: 14)
                    .matchedGeometryEffect(id: "crab", in: activityNamespace, isSource: !showClosedActivity)
                    .padding(.leading, 8)
            }

            HStack(spacing: 2) {
                navigationButton("Usage", symbol: "gauge.with.dots.needle.67percent", route: .usage) {
                    viewModel.showUsage()
                }
                navigationButton("Statistics", symbol: "chart.bar.xaxis", route: .stats) {
                    viewModel.showStats()
                }
                navigationButton("Settings", symbol: "gearshape", route: .menu) {
                    viewModel.showSettings()
                }
            }

            Spacer(minLength: 4)

            Button { viewModel.notchClose() } label: {
                Image(systemName: "xmark")
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(.secondary)
                    .frame(width: 24, height: 24)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Close Llmux Islands")
        }
    }

    private func navigationButton(
        _ label: String,
        symbol: String,
        route: NotchContentType,
        action: @escaping () -> Void
    ) -> some View {
        let selected = viewModel.contentType == route
        return Button(action: action) {
            Label(label, systemImage: symbol)
                .font(.caption.weight(selected ? .semibold : .regular))
                .foregroundStyle(selected ? Color.white : Color.white.opacity(0.55))
                .padding(.horizontal, 8)
                .padding(.vertical, 5)
                .background(Color.white.opacity(selected ? 0.12 : 0))
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(selected ? .isSelected : [])
    }

    @ViewBuilder
    private var contentView: some View {
        Group {
            switch viewModel.contentType {
            case .usage:
                IslandUsageView(
                    model: IslandUsageModel.shared,
                    viewModel: viewModel,
                    advancedInitiallyPresented: snapshotAdvancedInitiallyPresented,
                    snapshotNow: snapshotNow
                )
            case .stats:
                IslandStatsView(
                    model: IslandUsageModel.shared,
                    viewModel: viewModel,
                    snapshotNow: snapshotNow,
                    advancedInitiallyPresented: snapshotAdvancedInitiallyPresented,
                    advancedInitialPage: snapshotStatisticsPage
                )
            case .menu:
                NotchMenuView(
                    viewModel: viewModel,
                    advancedInitiallyPresented: snapshotAdvancedInitiallyPresented,
                    snapshotState: snapshotNow.flatMap { _ in
                        usageModel.canonicalState.map(NotchMenuSnapshotState.init(canonicalState:))
                    }
                )
            }
        }
        .frame(width: notchSize.width - 24)
    }

    // MARK: - Event Handlers

    private func handleStatusChange(from oldStatus: NotchStatus, to newStatus: NotchStatus) {
        switch newStatus {
        case .opened, .popping:
            isVisible = true
        case .closed:
            guard viewModel.hasPhysicalNotch else { return }
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.35) {
                if viewModel.status == .closed && !activityCoordinator.expandingActivity.show {
                    isVisible = false
                }
            }
        }
    }
}
