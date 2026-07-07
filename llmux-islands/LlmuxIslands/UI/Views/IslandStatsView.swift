import SwiftUI

/// The `.stats` content of the floating island (issue #68 v2): the #62
/// analytics — overview totals, models, clients, health — reached through the
/// ☰ menu's "Statistics" entry. The default panel stays the v0.2.14 Usage
/// tile view; this panel renders ONLY when the daemon serves
/// `/llmux/dashboard` (0.2.15+) and shows a quiet empty state otherwise.
///
/// Header + connection badge mirror `IslandUsageView` so both panels read as
/// the same app; the section switcher is the app's neutral+amber capsule
/// control (`StatsSegmentedControl`), never the system blue segmented picker.
struct IslandStatsView: View {
    @ObservedObject var model: IslandUsageModel
    @ObservedObject var viewModel: NotchViewModel

    @State private var section: StatsSection = .overview
    @State private var now = Date()
    private let clock = Timer.publish(every: 1, on: .main, in: .common).autoconnect()

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            header
            StatsSegmentedControl(selection: $section)
            content
        }
        .onReceive(clock) { now = $0 }
    }

    private var header: some View {
        HStack(spacing: 8) {
            Text("Statistics")
                .font(.system(size: 15, weight: .semibold))
                .foregroundColor(.white)
            connectionBadge
            Spacer()
            iconButton("arrow.clockwise") { Task { await model.refresh() } }
        }
        .padding(.horizontal, 2)
    }

    @ViewBuilder private var connectionBadge: some View {
        switch model.connection {
        case .connecting: badge(.white.opacity(0.4), "connecting…")
        case .online: badge(TerminalColors.green, "\(model.tiles.count)")
        case .offline: badge(TerminalColors.red, "offline")
        }
    }

    private func badge(_ color: Color, _ text: String) -> some View {
        HStack(spacing: 5) {
            Circle().fill(color).frame(width: 6, height: 6)
            Text(text)
                .font(.system(size: 10, design: .monospaced))
                .foregroundColor(.white.opacity(0.5))
        }
    }

    @ViewBuilder private var content: some View {
        if let dashboard = model.dashboard {
            ScrollView(.vertical, showsIndicators: false) {
                StatsSectionContent(
                    section: section,
                    dashboard: dashboard,
                    tiles: model.tiles,
                    now: now,
                    onRemoveAccount: { name in Task { await model.remove(name) } }
                )
                .padding(.bottom, 4)
            }
            .scrollBounceBehavior(.basedOnSize)
        } else if case .offline = model.connection {
            stateMessage(icon: "bolt.horizontal.circle",
                         title: "llmux not reachable",
                         detail: "start the daemon: llmux run  (:3456)",
                         tint: TerminalColors.red.opacity(0.85))
        } else {
            stateMessage(icon: "chart.bar",
                         title: "No statistics yet",
                         detail: "needs llmux 0.2.15+ (/llmux/dashboard)",
                         tint: .white.opacity(0.35))
        }
    }

    private func stateMessage(icon: String, title: String, detail: String, tint: Color) -> some View {
        VStack(spacing: 8) {
            Image(systemName: icon).font(.system(size: 26)).foregroundColor(tint)
            Text(title).foregroundColor(.white.opacity(0.7))
            Text(detail).font(.system(size: 10, design: .monospaced)).foregroundColor(.white.opacity(0.4))
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 24)
    }

    private func iconButton(_ symbol: String, _ action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: symbol)
                .font(.system(size: 11, weight: .semibold))
                .foregroundColor(.white.opacity(0.7))
                .frame(width: 24, height: 24)
                .background(RoundedRectangle(cornerRadius: 7).fill(Color.white.opacity(0.06)))
        }
        .buttonStyle(.plain)
    }
}
