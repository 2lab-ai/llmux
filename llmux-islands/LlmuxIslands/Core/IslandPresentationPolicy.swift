import Foundation

/// Presentation-only grouping for the native Islands surfaces.
///
/// This policy deliberately has no dependency on `SharedUiState`, semantic
/// actions, effects, or daemon DTOs. Advanced disclosure is ephemeral shell
/// state; revealing it must never look like a user operation to the shared
/// reducer.
enum IslandPresentationItem: CaseIterable {
    case navigation
    case connectionStatus
    case attentionReason
    case operationFailure
    case primaryQuota
    case summaryMetrics
    case accountOverview
    case addAccount
    case refresh
    case screen
    case sound
    case privacy
    case launchAtLogin

    case credentialMetadata
    case accountControls
    case analyticsDetail
    case requestReceipts
    case endpointCredentials
    case platformDiagnostics
    case events
    case maintenance
    case buildMetadata

    var isAdvanced: Bool {
        switch self {
        case .credentialMetadata, .accountControls, .analyticsDetail,
             .requestReceipts, .endpointCredentials, .platformDiagnostics,
             .events, .maintenance, .buildMetadata:
            true
        default:
            false
        }
    }
}

enum IslandPresentationPolicy {
    static let advancedLabel = "Advanced"

    static func isVisible(_ item: IslandPresentationItem, advancedPresented: Bool) -> Bool {
        !item.isAdvanced || advancedPresented
    }

    static func privateAccountLabel(providerName: String, ordinal: Int) -> String {
        "\(providerName) account \(max(1, ordinal))"
    }

    static func snapshotSurfaceFiles(emailAnonymous: Bool) -> [String] {
        [
            "menu.png",
            "menu-advanced.png",
            emailAnonymous ? "usage-anon-on.png" : "usage-anon-off.png",
            "usage-advanced.png",
            "stats.png",
            "stats-advanced.png",
            "receipts-detail.png",
        ]
    }
}
