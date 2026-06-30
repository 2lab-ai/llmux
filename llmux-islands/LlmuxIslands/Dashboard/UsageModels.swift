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
}

struct BucketUsageInfo: Decodable {
    let modelId: String
    let usedPercent: Double?
    let resetAt: Date?
}
