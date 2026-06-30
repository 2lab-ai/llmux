import SwiftUI

/// One account: provider badge, label + type, status dot, 5h/7d usage gauges,
/// reset countdown, and an inline-confirmed remove. Pure function of an
/// `LlmuxAccount` — all data comes from the llmux API.
struct AccountTile: View {
    let account: LlmuxAccount
    let isCurrent: Bool
    let onRemove: () -> Void

    @State private var confirmingRemove = false

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            header
            gauge("5h", account.fiveHour)
            gauge("7d", account.sevenDay)
            footer
        }
        .padding(12)
        .background(
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .fill(Color.white.opacity(isCurrent ? 0.10 : 0.05))
                .overlay(
                    RoundedRectangle(cornerRadius: 12, style: .continuous)
                        .stroke(
                            isCurrent ? TerminalColors.green.opacity(0.5) : Color.white.opacity(0.06),
                            lineWidth: 1
                        )
                )
        )
    }

    private var header: some View {
        HStack(spacing: 8) {
            ProviderIcon(provider: account.provider)
            VStack(alignment: .leading, spacing: 1) {
                Text(account.label)
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundColor(.white)
                    .lineLimit(1)
                    .truncationMode(.middle)
                Text(account.type)
                    .font(.system(size: 9, weight: .medium, design: .monospaced))
                    .foregroundColor(.white.opacity(0.4))
            }
            Spacer(minLength: 4)
            statusDot
        }
    }

    private var statusDot: some View {
        let color: Color
        switch account.status ?? "" {
        case "active": color = TerminalColors.green
        case "auth_failed": color = TerminalColors.red
        case "cooldown": color = TerminalColors.amber
        default: color = .white.opacity(0.25)
        }
        return Circle().fill(color).frame(width: 7, height: 7)
    }

    private func gauge(_ label: String, _ window: LlmuxWindow?) -> some View {
        HStack(spacing: 6) {
            Text(label)
                .font(.system(size: 9, weight: .semibold, design: .monospaced))
                .foregroundColor(.white.opacity(0.4))
                .frame(width: 18, alignment: .leading)
            if let window {
                GeometryReader { geo in
                    ZStack(alignment: .leading) {
                        Capsule().fill(Color.white.opacity(0.08))
                        Capsule()
                            .fill(gaugeColor(window.percent))
                            .frame(width: max(2, geo.size.width * CGFloat(window.percent) / 100))
                    }
                }
                .frame(height: 6)
                Text("\(window.percent)%")
                    .font(.system(size: 10, weight: .semibold, design: .monospaced))
                    .foregroundColor(.white.opacity(0.7))
                    .frame(width: 36, alignment: .trailing)
            } else {
                Text("no data")
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundColor(.white.opacity(0.25))
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }

    private func gaugeColor(_ pct: Int) -> Color {
        switch pct {
        case ..<60: return TerminalColors.green
        case 60..<85: return TerminalColors.amber
        default: return TerminalColors.red
        }
    }

    private var footer: some View {
        HStack(spacing: 8) {
            if let resets = account.fiveHour?.resetsInSecs {
                HStack(spacing: 3) {
                    Image(systemName: "arrow.clockwise")
                        .font(.system(size: 8))
                        .foregroundColor(.white.opacity(0.3))
                    UsageDurationText.make(seconds: resets)
                }
            }
            Spacer()
            if confirmingRemove {
                Button {
                    onRemove()
                    confirmingRemove = false
                } label: {
                    Text("Remove").font(.system(size: 10, weight: .semibold))
                }
                .buttonStyle(.plain)
                .foregroundColor(TerminalColors.red)
                Button {
                    confirmingRemove = false
                } label: {
                    Text("Cancel").font(.system(size: 10))
                }
                .buttonStyle(.plain)
                .foregroundColor(.white.opacity(0.5))
            } else {
                Button {
                    confirmingRemove = true
                } label: {
                    Image(systemName: "trash")
                        .font(.system(size: 10))
                        .foregroundColor(.white.opacity(0.35))
                }
                .buttonStyle(.plain)
            }
        }
    }
}
