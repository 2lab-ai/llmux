import SwiftUI

/// The common Statistics path contains only summary metrics, account overview,
/// and safety-relevant health. Models, clients, heat, activity, health detail,
/// and receipts are local Advanced presentation.
struct IslandStatsView: View {
    @ObservedObject var model: IslandUsageModel
    @ObservedObject var viewModel: NotchViewModel

    @State private var advancedPresented: Bool
    @State private var now: Date
    private let snapshotNow: Date?
    private let advancedInitialPage: StatisticsAdvancedPage
    private let clock = Timer.publish(every: 1, on: .main, in: .common).autoconnect()

    init(
        model: IslandUsageModel,
        viewModel: NotchViewModel,
        snapshotNow: Date? = nil,
        advancedInitiallyPresented: Bool = false,
        advancedInitialPage: StatisticsAdvancedPage = .analytics
    ) {
        self.model = model
        self.viewModel = viewModel
        self.snapshotNow = snapshotNow
        self.advancedInitialPage = advancedInitialPage
        _advancedPresented = State(initialValue: advancedInitiallyPresented)
        _now = State(initialValue: snapshotNow ?? Date())
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            header
            if case .offline(let reason) = model.connection {
                IslandSafetyBanner(title: "llmux is offline", detail: reason, critical: true)
            }
            IslandLatestFailureBanner(receipts: model.verificationReceipts)
            content
        }
        .onReceive(clock) {
            if snapshotNow == nil { now = $0 }
        }
    }

    private var header: some View {
        HStack(alignment: .center, spacing: 10) {
            VStack(alignment: .leading, spacing: 2) {
                Text("Statistics")
                    .font(.headline)
                Text("Summary across llmux accounts")
                    .font(.caption)
                    .foregroundColor(.white.opacity(0.6))
            }
            IslandConnectionLabel(connection: model.connection, accountCount: model.tiles.count)
            Spacer()
            Button { Task { await model.refresh() } } label: {
                Label("Refresh", systemImage: "arrow.clockwise")
                    .font(.caption.weight(.medium))
            }
            .buttonStyle(IslandButtonStyle())
        }
        .padding(.horizontal, 2)
    }

    @ViewBuilder private var content: some View {
        if let dashboard = model.dashboard {
            ScrollView(.vertical, showsIndicators: false) {
                VStack(alignment: .leading, spacing: 10) {
                    let health = DashboardHealth.summary(dashboard.accounts)
                    if health.isWarning {
                        UsageHealthBanner(summary: health)
                    }

                    IslandSurface {
                        VStack(alignment: .leading, spacing: 8) {
                            Text("Summary")
                                .font(.subheadline.weight(.semibold))
                            UsageSummaryCards(
                                totals: dashboard.totals,
                                costCaption: DataQualityLabels(dashboard.dataQuality).cost
                            )
                        }
                    }

                    if !model.tiles.isEmpty {
                        IslandSurface {
                            VStack(alignment: .leading, spacing: 8) {
                                Text("Accounts")
                                    .font(.subheadline.weight(.semibold))
                                UsageAccountCompactList(tiles: model.tiles)
                            }
                        }
                    }

                    IslandAdvancedDisclosure(isPresented: $advancedPresented) {
                        StatisticsAdvancedContent(
                            dashboard: dashboard,
                            tiles: model.tiles,
                            now: now,
                            activityReceipts: model.activityReceipts,
                            verificationReceipts: model.verificationReceipts,
                            initialPage: advancedInitialPage
                        )
                    }
                }
                .padding(.bottom, 4)
            }
            .scrollBounceBehavior(.basedOnSize)
        } else if case .offline = model.connection {
            stateMessage(icon: "bolt.horizontal.circle",
                         title: "llmux not reachable",
                         detail: "check the configured llmux endpoint and credentials",
                         tint: TerminalColors.red.opacity(0.85))
        } else {
            stateMessage(icon: "chart.bar",
                         title: "No statistics yet",
                         detail: "needs llmux 0.2.15+ (/llmux/dashboard)",
                         tint: .white.opacity(0.5))
        }
    }

    private func stateMessage(icon: String, title: String, detail: String, tint: Color) -> some View {
        VStack(spacing: 8) {
            Image(systemName: icon).font(.system(size: 26)).foregroundColor(tint)
            Text(title).foregroundColor(.white.opacity(0.7))
            Text(detail).font(.caption).foregroundColor(.white.opacity(0.6))
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 24)
    }

}

enum StatisticsAdvancedPage: String, CaseIterable {
    case analytics = "Analytics"
    case receipts = "Activity & receipts"
}

/// Production Advanced content, also used by snapshot mode so receipt evidence
/// proves the same discoverable route that users open in the live panel.
struct StatisticsAdvancedContent: View {
    let dashboard: LlmuxDashboard
    let tiles: [UsageAccountTile]
    let now: Date
    let activityReceipts: [SharedActivityReceipt]
    let verificationReceipts: [SharedVerificationReceipt]

    @State private var page: StatisticsAdvancedPage
    @State private var section: StatsSection = .overview

    init(
        dashboard: LlmuxDashboard,
        tiles: [UsageAccountTile],
        now: Date,
        activityReceipts: [SharedActivityReceipt],
        verificationReceipts: [SharedVerificationReceipt],
        initialPage: StatisticsAdvancedPage = .analytics
    ) {
        self.dashboard = dashboard
        self.tiles = tiles
        self.now = now
        self.activityReceipts = activityReceipts
        self.verificationReceipts = verificationReceipts
        _page = State(initialValue: initialPage)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 3) {
                ForEach(StatisticsAdvancedPage.allCases, id: \.self) { option in
                    let selected = option == page
                    Button {
                        page = option
                    } label: {
                        Text(option.rawValue)
                            .font(.caption.weight(selected ? .semibold : .regular))
                            .foregroundStyle(selected ? Color.black : Color.white.opacity(0.6))
                            .frame(maxWidth: .infinity)
                            .frame(minHeight: 44)
                            .background(selected ? Color.white : Color.clear)
                    }
                    .buttonStyle(.plain)
                }
            }
            .overlay(alignment: .bottom) {
                Rectangle().fill(Color.white.opacity(0.12)).frame(height: 1)
            }

            switch page {
            case .analytics:
                StatsSegmentedControl(selection: $section)
                StatsSectionContent(
                    section: section,
                    dashboard: dashboard,
                    tiles: tiles,
                    now: now,
                    includePrimaryOverview: false
                )
            case .receipts:
                VStack(alignment: .leading, spacing: 10) {
                    StatsSectionLabel("Recent activity")
                    if activityReceipts.isEmpty {
                        UsageActivityList(activity: dashboard.activity, now: now)
                    } else {
                        UsageCanonicalActivityReceiptList(receipts: activityReceipts, now: now)
                    }
                    if !verificationReceipts.isEmpty {
                        StatsSectionLabel("Verification receipts")
                        UsageVerificationReceiptList(receipts: verificationReceipts, now: now)
                    }
                }
            }
        }
    }
}
