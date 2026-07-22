import Foundation

// Value types lifted verbatim from agent-island's Services/Usage layer, kept as
// the DTOs the dashboard tiles (UsageTiles.swift) bind to. In llmux-islands the
// data is synthesized from the llmux HTTP API (Llmux/IslandUsageModel) instead
// of the original cauth/credential pipeline, so the fetcher/profile/credential
// machinery is dropped and only these plain value types remain.
//
// CheckUsageOutput / CLIUsageInfo / BucketUsageInfo live in Dashboard/UsageModels.swift.

// from Services/Usage/ProfileStore.swift
struct UsageProfile: Codable, Equatable, Identifiable {
    let name: String
    let claudeAccountId: String?
    let codexAccountId: String?
    let geminiAccountId: String?
    var id: String { name }
}

// from Services/Usage/AccountStore.swift
enum UsageService: String, Codable {
    case claude
    case codex
    case gemini
}

// from Services/Usage/UsageFetcher.swift
struct UsageIdentities: Sendable {
    let claudeEmail: String?
    let claudeTier: String?
    let claudeIsTeam: Bool?
    let codexEmail: String?
    let geminiEmail: String?

    static let empty = UsageIdentities(
        claudeEmail: nil, claudeTier: nil, claudeIsTeam: nil,
        codexEmail: nil, geminiEmail: nil
    )
}

struct TokenRefreshInfo: Sendable {
    let expiresAt: Date
    let lifetimeSeconds: TimeInterval
}

struct UsageTokenRefresh: Sendable {
    let claude: TokenRefreshInfo?
    let codex: TokenRefreshInfo?
    let gemini: TokenRefreshInfo?

    static let empty = UsageTokenRefresh(claude: nil, codex: nil, gemini: nil)
}

enum UsageIssueKind: Sendable, Equatable {
    case cauthUnavailable
    case cauthExecutionFailed
    case cauthOutputInvalid
    case credentialsMissing
}

struct UsageIssue: Sendable, Equatable {
    let kind: UsageIssueKind
    let message: String
    let technicalDetails: String?
}

struct UsageSnapshot: Sendable, Identifiable {
    let profileName: String
    let output: CheckUsageOutput?
    let identities: UsageIdentities
    let tokenRefresh: UsageTokenRefresh
    let fetchedAt: Date?
    let isStale: Bool
    let errorMessage: String?
    let issue: UsageIssue?

    var id: String { profileName }
}

// from UI/Views/UsageDashboardView.swift (view-model helper)
struct UsageAccountIdSet: Sendable, Equatable {
    let claude: String?
    let codex: String?
    let gemini: String?

    static let empty = UsageAccountIdSet(claude: nil, codex: nil, gemini: nil)
    var hasAny: Bool { claude != nil || codex != nil || gemini != nil }
    func matches(profile: UsageProfile) -> Bool {
        claude == profile.claudeAccountId &&
            codex == profile.codexAccountId &&
            gemini == profile.geminiAccountId
    }
}

// Minimal stub for the dropped Claude-Code-token feature. The lifted tile takes
// this as an optional; llmux-islands never sets it, so the fields suffice for
// the tile's `isSet` / `isEnabled` reads.
struct ClaudeCodeTokenStatus {
    let isSet: Bool
    let isEnabled: Bool
}
