import AppKit
import SwiftUI

struct UsageProviderIcon: View {
    let provider: UsageProvider
    var size: CGFloat = 14

    var body: some View {
        Group {
            if let image = UsageProviderIconCache.shared.image(for: provider) {
                Image(nsImage: image)
                    .resizable()
                    .scaledToFit()
            } else {
                Text(String(provider.displayName.prefix(1)))
                    .font(.system(size: size * 0.72, weight: .semibold, design: .rounded))
                    .foregroundColor(.white.opacity(0.75))
                    .frame(width: size, height: size)
                    .background(
                        RoundedRectangle(cornerRadius: size * 0.28, style: .continuous)
                            .fill(Color.white.opacity(0.08))
                    )
            }
        }
        .frame(width: size, height: size, alignment: .center)
        .accessibilityLabel(Text(provider.displayName))
        .accessibilityAddTraits(.isImage)
    }
}

private final class UsageProviderIconCache {
    static let shared = UsageProviderIconCache()

    private var cache: [UsageProvider: NSImage] = [:]

    func image(for provider: UsageProvider, bundle: Bundle = .main) -> NSImage? {
        if let cached = cache[provider] { return cached }

        let resourceName: String
        let resourceExtension: String
        switch provider {
        case .claude:
            resourceName = "claude"
            resourceExtension = "svg"
        case .codex:
            resourceName = "codex"
            resourceExtension = "svg"
        case .gemini:
            resourceName = "gemini"
            resourceExtension = "png"
        }

        guard let url = bundle.url(forResource: resourceName, withExtension: resourceExtension)
            ?? bundle.url(forResource: resourceName, withExtension: resourceExtension, subdirectory: "assets")
        else {
            return nil
        }

        guard let image = NSImage(contentsOf: url) else { return nil }
        cache[provider] = image
        return image
    }
}

