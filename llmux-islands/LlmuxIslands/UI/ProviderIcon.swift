import SwiftUI

/// Small provider badge. Uses SF Symbols (reliable across machines) tinted per
/// provider; the bundled `claude.svg` / `codex.svg` assets are kept in
/// Resources for a future brand-faithful variant.
struct ProviderIcon: View {
    let provider: LlmuxProvider
    var size: CGFloat = 16

    private var symbol: String {
        switch provider {
        case .claude: return "sparkles"
        case .codex: return "chevron.left.forwardslash.chevron.right"
        case .unknown: return "circle.dotted"
        }
    }

    private var tint: Color {
        switch provider {
        case .claude: return TerminalColors.prompt
        case .codex: return TerminalColors.green
        case .unknown: return .white.opacity(0.5)
        }
    }

    var body: some View {
        Image(systemName: symbol)
            .font(.system(size: size * 0.75, weight: .semibold))
            .foregroundColor(tint)
            .frame(width: size + 9, height: size + 9)
            .background(
                RoundedRectangle(cornerRadius: 7, style: .continuous)
                    .fill(tint.opacity(0.15))
            )
    }
}
