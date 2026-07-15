import SwiftUI

struct IslandButtonStyle: ButtonStyle {
    enum Emphasis { case filled, outline, quiet }

    let emphasis: Emphasis
    @Environment(\.isEnabled) private var isEnabled

    init(_ emphasis: Emphasis = .outline) {
        self.emphasis = emphasis
    }

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .foregroundColor(foreground)
            .padding(.horizontal, 12)
            .frame(minHeight: 44)
            .background(background.opacity(configuration.isPressed ? 0.72 : 1))
            .overlay {
                if emphasis == .outline {
                    Rectangle().stroke(Color.white.opacity(0.22), lineWidth: 1)
                }
            }
            .opacity(isEnabled ? 1 : 0.44)
            .contentShape(Rectangle())
    }

    private var foreground: Color {
        emphasis == .filled ? .black : .white.opacity(0.84)
    }

    private var background: Color {
        switch emphasis {
        case .filled: .white
        case .outline: .white.opacity(0.04)
        case .quiet: .clear
        }
    }
}

/// Flat inverted OpenAI-reference surface: black canvas, white ink, and a
/// single hairline. Internal panels stay square; the outer notch silhouette is
/// the only platform-required rounded shape.
struct IslandSurface<Content: View>: View {
    private let content: Content

    init(@ViewBuilder content: () -> Content) {
        self.content = content()
    }

    var body: some View {
        content
            .padding(.vertical, 12)
            .overlay(alignment: .top) {
                Rectangle().fill(Color.white.opacity(0.12)).frame(height: 1)
            }
    }
}

/// A local-only disclosure used consistently across Usage, Statistics, and
/// Settings. It intentionally performs no animation and dispatches no model
/// action; the binding belongs to the presenting view.
struct IslandAdvancedDisclosure<Content: View>: View {
    @Binding var isPresented: Bool
    private let content: Content

    init(isPresented: Binding<Bool>, @ViewBuilder content: () -> Content) {
        _isPresented = isPresented
        self.content = content()
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Button {
                isPresented.toggle()
            } label: {
                HStack(spacing: 8) {
                    Label(IslandPresentationPolicy.advancedLabel, systemImage: "slider.horizontal.3")
                        .font(.subheadline.weight(.medium))
                    Spacer()
                    Text(isPresented ? "Hide" : "Show")
                        .font(.caption)
                        .foregroundColor(isPresented ? .black.opacity(0.6) : .white.opacity(0.6))
                    Image(systemName: isPresented ? "chevron.up" : "chevron.down")
                        .font(.caption2.weight(.semibold))
                        .foregroundColor(isPresented ? .black.opacity(0.6) : .white.opacity(0.5))
                }
                .foregroundColor(isPresented ? .black : .white.opacity(0.6))
                .frame(minHeight: 44)
                .padding(.horizontal, 10)
                .background(isPresented ? Color.white : Color.white.opacity(0.04))
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityValue(isPresented ? "Expanded" : "Collapsed")

            if isPresented {
                content
            }
        }
        .overlay(alignment: .top) {
            Rectangle().fill(Color.white.opacity(0.12)).frame(height: 1)
        }
    }
}

struct IslandConnectionLabel: View {
    let connection: IslandUsageModel.Connection
    let accountCount: Int

    var body: some View {
        HStack(spacing: 6) {
            Circle()
                .fill(indicatorColor)
                .frame(width: 6, height: 6)
            Text(label)
                .font(.caption)
                .foregroundColor(.white.opacity(0.6))
        }
        .accessibilityElement(children: .combine)
    }

    private var indicatorColor: Color {
        switch connection {
        case .connecting: .white.opacity(0.35)
        case .online: .white.opacity(0.72)
        case .offline: TerminalColors.red
        }
    }

    private var label: String {
        switch connection {
        case .connecting: "Connecting"
        case .online: accountCount == 1 ? "1 account" : "\(accountCount) accounts"
        case .offline: "Offline"
        }
    }
}

struct IslandSafetyBanner: View {
    let title: String
    let detail: String?
    var critical = false

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: critical ? "exclamationmark.octagon.fill" : "exclamationmark.triangle.fill")
                .foregroundStyle(critical ? TerminalColors.red : TerminalColors.amber)
            VStack(alignment: .leading, spacing: 2) {
                Text(title)
                    .font(.subheadline.weight(.semibold))
                if let detail, !detail.isEmpty {
                    Text(detail)
                        .font(.caption)
                        .foregroundColor(.white.opacity(0.6))
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            Spacer(minLength: 0)
        }
        .padding(.vertical, 10)
        .padding(.horizontal, 12)
        .background(Color.white.opacity(0.04))
        .overlay(alignment: .leading) {
            Rectangle()
                .fill(critical ? TerminalColors.red : TerminalColors.amber)
                .frame(width: 2)
        }
    }
}

/// A failed operation remains actionable even when its originating technical
/// controls are collapsed. A newer non-failure receipt clears the beacon;
/// complete history remains in Statistics Advanced.
struct IslandLatestFailureBanner: View {
    let receipts: [SharedVerificationReceipt]

    @ViewBuilder var body: some View {
        if let latest = receipts.last, latest.outcome == "failed" {
            IslandSafetyBanner(
                title: "Last operation failed",
                detail: latest.message,
                critical: true
            )
        }
    }
}
