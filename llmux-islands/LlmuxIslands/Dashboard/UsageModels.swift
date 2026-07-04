import Foundation

struct CheckUsageOutput: Decodable {
    let claude: CLIUsageInfo
    let codex: CLIUsageInfo?
    let gemini: CLIUsageInfo?
    let zai: CLIUsageInfo?
    let recommendation: String?
    let recommendationReason: String
}

struct CLIUsageInfo: Decodable {
    let name: String
    let available: Bool
    let error: Bool
    let fiveHourPercent: Double?
    let sevenDayPercent: Double?
    let fiveHourReset: Date?
    let sevenDayReset: Date?
    let model: String?
    let plan: String?
    let buckets: [BucketUsageInfo]?
    // Fable weekly (7d) window, synthesized from the daemon's `fable_weekly`
    // (nil when the account has no Fable weekly limit or the daemon predates
    // the field). Percent is 0...100 like the 5h/7d fields; the reset-aware
    // `constraining` bool drives the tile's red emphasis (severity/is_active are
    // kept for context). Defaulted so the two memberwise-init call sites
    // (IslandUsageModel, SnapshotMode) and the synthesized Decodable init stay
    // source-compatible.
    var fableWeeklyPercent: Double? = nil
    var fableWeeklyReset: Date? = nil
    var fableWeeklySeverity: String? = nil
    var fableWeeklyIsActive: Bool? = nil
    var fableWeeklyConstraining: Bool? = nil
}

struct BucketUsageInfo: Decodable {
    let modelId: String
    let usedPercent: Double?
    let resetAt: Date?
}
