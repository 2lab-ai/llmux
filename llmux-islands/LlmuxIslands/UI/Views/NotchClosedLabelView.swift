//
//  NotchClosedLabelView.swift
//  LlmuxIslands
//
//  Closed-island pill (issue #68 cleanup of todo.md items 1–2):
//
//      llmux [⚠N] [C{n}] [X{m}] [· $cost]
//
//  Segment rules live in `ClosedPillSegments` (DashboardAnalytics.swift — the
//  testable single source): zero in-flight counters are HIDDEN (no `C:0 X:0`
//  noise), the warning badge carries the affected-account count, the cost
//  segment is omitted when the daemon reports none, and the old decorative
//  icons (mascot, provider glyphs) are gone. Idle example: `llmux ⚠5 · $9.1k`.
//
//  While sessions are in flight the counters cycle through rainbow hues in a
//  continuous loop — the live view drives the hue from a TimelineView clock;
//  offscreen snapshot mode (SnapshotMode.swift) renders it at fixed phases.
//

import Foundation
import SwiftUI

struct NotchClosedLabelView: View {
    /// Σ in-flight sessions over Claude accounts — drives the `C{n}` segment.
    let claudeCount: Int
    /// Σ in-flight sessions over Codex accounts — drives the `X{m}` segment.
    let codexCount: Int
    /// `totals.cost_usd` from the dashboard doc. nil (old daemon / status
    /// fallback) omits the segment — never renders a fabricated $0.00.
    let sessionCost: Double?
    /// U13 hard rule: number of accounts that are `auth_failed` or over 90%
    /// quota — the SAME `DashboardHealth` source as the banner. 0 hides the
    /// `⚠N` badge.
    let warningCount: Int
    /// Whether the island is actually on screen (NotchView's `isVisible`). On
    /// notched Macs the closed pill sits at opacity 0 until hovered — keep the
    /// 30fps timeline paused then instead of animating an invisible view.
    let active: Bool

    /// Full rainbow revolution takes this long.
    private static let rainbowLoopSeconds: Double = 3.0

    /// Hue offsets so the two providers don't share the exact same color.
    static let claudeHueSeed: Double = 0
    static let codexHueSeed: Double = 0.35

    private var isAnimating: Bool { claudeCount > 0 || codexCount > 0 }

    var body: some View {
        TimelineView(.animation(minimumInterval: 1.0 / 30.0, paused: !(isAnimating && active))) { timeline in
            let time = timeline.date.timeIntervalSinceReferenceDate
            NotchClosedLabelContent(
                claudeCount: claudeCount,
                codexCount: codexCount,
                sessionCost: sessionCost,
                warningCount: warningCount,
                claudeHue: Self.rainbowHue(time: time, seed: Self.claudeHueSeed),
                codexHue: Self.rainbowHue(time: time, seed: Self.codexHueSeed)
            )
        }
    }

    // MARK: - Rainbow

    /// Continuous 0..<1 hue loop from wall-clock time.
    static func rainbowHue(time: TimeInterval, seed: Double) -> Double {
        rainbowHue(phase: time / rainbowLoopSeconds, seed: seed)
    }

    /// Hue for a fixed 0..<1 phase (snapshot mode renders these directly).
    static func rainbowHue(phase: Double, seed: Double) -> Double {
        let hue = (phase + seed).truncatingRemainder(dividingBy: 1)
        return hue < 0 ? hue + 1 : hue
    }
}

/// The pill row itself — a pure function of the segment struct + hue phases,
/// shared by the live TimelineView wrapper above and offscreen snapshots.
struct NotchClosedLabelContent: View {
    let claudeCount: Int
    let codexCount: Int
    /// Session cost segment (`$0.42`); nil = daemon reports none → omitted.
    let sessionCost: Double?
    /// Warning-state account count: > 0 renders `⚠N` and paints the pill in
    /// the warning color (the rainbow is suppressed — the color IS the signal).
    let warningCount: Int
    /// 0..<1 rainbow hue for the `C{n}` segment.
    let claudeHue: Double
    /// 0..<1 rainbow hue for the `X{m}` segment.
    let codexHue: Double

    private var segments: ClosedPillSegments {
        ClosedPillSegments(
            claude: claudeCount, codex: codexCount,
            warningCount: warningCount, costUsd: sessionCost
        )
    }

    private var warning: Bool { segments.warningCount != nil }

    var body: some View {
        HStack(spacing: 7) {
            // Prefix text. If space ever gets tight, shrink/truncate this
            // (never the counts) — see minimumScaleFactor + tail truncation.
            Text(ClosedPillSegments.prefix)
                .font(.system(size: 11, weight: .semibold, design: .rounded))
                .foregroundColor(warning ? TerminalColors.amber : .white.opacity(0.85))
                .lineLimit(1)
                .truncationMode(.tail)
                .minimumScaleFactor(0.6)

            if let count = segments.warningCount {
                HStack(spacing: 2) {
                    Image(systemName: "exclamationmark.triangle.fill")
                        .font(.system(size: 9, weight: .semibold))
                    Text("\(count)")
                        .font(.system(size: 11, weight: .bold, design: .rounded))
                        .monospacedDigit()
                }
                .foregroundColor(TerminalColors.amber)
            }

            // In-flight counters — hidden entirely at 0 (#68), rainbow while
            // active and healthy.
            if let claude = segments.claude {
                counter("C\(claude)", hue: claudeHue)
            }
            if let codex = segments.codex {
                counter("X\(codex)", hue: codexHue)
            }

            if let cost = segments.cost {
                Text("·")
                    .font(.system(size: 11, weight: .bold, design: .rounded))
                    .foregroundColor(.white.opacity(0.35))
                Text(cost)
                    .font(.system(size: 11, weight: .bold, design: .rounded))
                    .monospacedDigit()
                    .foregroundColor(warning ? TerminalColors.amber : .white.opacity(0.7))
            }
        }
    }

    private func counter(_ text: String, hue: Double) -> some View {
        Text(text)
            .font(.system(size: 11, weight: .bold, design: .rounded))
            .monospacedDigit()
            .foregroundColor(
                warning
                    ? TerminalColors.amber
                    : Color(hue: hue, saturation: 0.85, brightness: 1.0)
            )
    }
}
