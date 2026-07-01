import Foundation

/// Demo/recording mode for screen-recorded walkthroughs.
///
/// Active when the app is launched with `--demo` or `LLMUX_ISLANDS_DEMO=1`.
/// In this mode the island **opens itself and stays open** (so a recorder can
/// capture the usage panel without a human hovering the notch), and account
/// emails are replaced with **stable fake addresses** so a public demo GIF never
/// leaks real account names. This mirrors the CLI/daemon `LLMUX_DEMO_MODE=1`
/// behaviour that masks emails in the dashboard/status/logs.
enum DemoMode {
    static let isActive: Bool =
        CommandLine.arguments.contains("--demo")
        || ProcessInfo.processInfo.environment["LLMUX_ISLANDS_DEMO"] == "1"

    /// Stable fake email for the account at `index` (0-based) in the tile list.
    /// Deterministic per position so labels don't flicker between status polls.
    static func fakeEmail(index: Int) -> String {
        "demo-\(index + 1)@example.com"
    }
}
