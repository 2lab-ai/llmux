//
//  NotchClosedLabelView.swift
//  LlmuxIslands
//
//  Closed-island label (todo.md items 1–2). Replaces the plain black box with
//
//      Llmux Islands [mascot] [claude]{n} [codex]{m} [grok]{k} [gemini]{g}
//
//  - `[mascot]` is the existing pixel-art ClaudeCrabIcon (the app's top-left
//    element), looping a small vertical jump whose speed scales with the Claude
//    session count: 1 = normal, faster from 2, very fast at 10, clamped past 10.
//  - A provider group `[icon]{count}` is hidden entirely while its count is 0;
//    at ≥1 it cycles through rainbow hues in a continuous loop.
//
//  Layout/colors live in `NotchClosedLabelContent`, a pure function of counts
//  and animation phases — the live view drives it from a TimelineView clock;
//  offscreen snapshot mode (SnapshotMode.swift) renders it at fixed phases.
//

import Foundation
import SwiftUI

struct NotchClosedLabelView: View {
    /// Σ in-flight sessions over Claude accounts — drives `{n}` and the jump.
    let claudeCount: Int
    /// Σ in-flight sessions over Codex accounts — drives `{m}`.
    let codexCount: Int
    /// Σ in-flight sessions over Grok accounts — drives `{k}`.
    let grokCount: Int
    /// Σ in-flight sessions over Gemini accounts — drives `{g}`.
    let geminiCount: Int
    /// The worst live exception. Only `.offline` shows on the closed label
    /// (warnings live in NEEDS ATTENTION since 2026-11-27).
    var signal: GlanceSignal = .none
    /// Whether the island is actually on screen (NotchView's `isVisible`);
    /// the 30fps timeline stays paused while it is not.
    let active: Bool
    /// See `NotchClosedLabelContent.notchGap`.
    var notchGap: CGFloat? = nil

    /// Full rainbow revolution takes this long.
    private static let rainbowLoopSeconds: Double = 3.0
    /// Jump cycle duration at 1 Claude session (normal speed).
    private static let slowestJumpPeriod: Double = 1.2
    /// Jump cycle duration at ≥10 Claude sessions (very fast, clamped).
    private static let fastestJumpPeriod: Double = 0.25
    /// Leading fraction of the jump cycle spent airborne (rest = on the ground).
    private static let airborneFraction: Double = 0.6
    /// How high the mascot hops, in points ("살짝").
    private static let jumpHeight: CGFloat = 3

    /// Hue offsets so the providers don't share the exact same color.
    static let claudeHueSeed: Double = 0
    static let codexHueSeed: Double = 0.35
    static let grokHueSeed: Double = 0.7
    static let geminiHueSeed: Double = 0.525

    private var isAnimating: Bool { claudeCount > 0 || codexCount > 0 || grokCount > 0 || geminiCount > 0 }

    var body: some View {
        TimelineView(.animation(minimumInterval: 1.0 / 30.0, paused: !(isAnimating && active))) { timeline in
            let time = timeline.date.timeIntervalSinceReferenceDate
            NotchClosedLabelContent(
                claudeCount: claudeCount,
                codexCount: codexCount,
                grokCount: grokCount,
                geminiCount: geminiCount,
                jumpOffset: Self.jumpOffset(time: time, claudeSessions: claudeCount),
                claudeHue: Self.rainbowHue(time: time, seed: Self.claudeHueSeed),
                codexHue: Self.rainbowHue(time: time, seed: Self.codexHueSeed),
                grokHue: Self.rainbowHue(time: time, seed: Self.grokHueSeed),
                geminiHue: Self.rainbowHue(time: time, seed: Self.geminiHueSeed),
                signal: signal,
                notchGap: notchGap
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

    // MARK: - Jump

    /// Vertical offset for the mascot's hop at `time`. 0 (grounded) when no
    /// Claude sessions are running.
    static func jumpOffset(time: TimeInterval, claudeSessions: Int) -> CGFloat {
        guard let period = jumpPeriod(claudeSessions: claudeSessions) else { return 0 }
        let phase = time.truncatingRemainder(dividingBy: period) / period
        return jumpOffset(phase: phase, claudeSessions: claudeSessions)
    }

    /// Vertical offset at a fixed 0..<1 position within the jump cycle
    /// (snapshot mode renders these directly).
    static func jumpOffset(phase: Double, claudeSessions: Int) -> CGFloat {
        guard claudeSessions >= 1 else { return 0 }
        let normalized = phase - floor(phase)
        guard normalized < airborneFraction else { return 0 }
        return -jumpHeight * CGFloat(sin(.pi * normalized / airborneFraction))
    }

    /// Jump cycle duration for a Claude session count: nil (idle) at 0, normal
    /// speed at 1, linearly faster up to 10, clamped for anything past 10.
    static func jumpPeriod(claudeSessions: Int) -> TimeInterval? {
        guard claudeSessions >= 1 else { return nil }
        let clamped = Double(min(claudeSessions, 10))
        return slowestJumpPeriod - (slowestJumpPeriod - fastestJumpPeriod) * (clamped - 1) / 9.0
    }
}

/// The label row itself — a pure function of counts + animation phases, shared
/// by the live TimelineView wrapper above and offscreen snapshot rendering.
struct NotchClosedLabelContent: View {
    let claudeCount: Int
    let codexCount: Int
    let grokCount: Int
    let geminiCount: Int
    /// Mascot vertical offset in points (≤ 0 while airborne).
    let jumpOffset: CGFloat
    /// 0..<1 rainbow hue for the claude `[icon]{n}` group.
    let claudeHue: Double
    /// 0..<1 rainbow hue for the codex `[icon]{m}` group.
    let codexHue: Double
    /// 0..<1 rainbow hue for the grok `[icon]{k}` group.
    let grokHue: Double
    /// 0..<1 rainbow hue for the gemini `[icon]{g}` group.
    let geminiHue: Double
    /// Only `.offline` changes the label: an `offline` chip replaces the
    /// counts and the mascot dims. Warnings are not shown here.
    var signal: GlanceSignal = .none
    /// Width of a physical notch the label must not draw under, or nil on a
    /// display without one. With a notch the label's items split around it
    /// (NotchSplitLayout), so nothing hides behind the hardware.
    var notchGap: CGFloat? = nil

    var body: some View {
        if let notchGap {
            // Mascot, activity and chip in one row that the layout splits
            // around the notch so both wings come out about the same width
            // (a wide right wing next to a lone mascot left the left wing an
            // empty black bar). Equal wings keep the notch centred.
            NotchSplitLayout(gap: notchGap + 16, spacing: 8) {
                mascot
                activityItems
            }
        } else {
            HStack(spacing: 8) {
                leftWing
                rightWing
            }
        }
    }

    private var isOffline: Bool {
        if case .offline = signal { return true }
        return false
    }

    /// Title + mascot. Beside a physical notch the title is left out: the
    /// wings cover menu bar space, so they stay as narrow as their content.
    @ViewBuilder
    private var leftWing: some View {
        HStack(spacing: 8) {
            if notchGap == nil {
                Text("Llmux Islands")
                    .font(.system(size: 11, weight: .semibold, design: .rounded))
                    .foregroundColor(.white.opacity(0.85))
                    .lineLimit(1)
                    .truncationMode(.tail)
                    .minimumScaleFactor(0.6)
            }

            mascot
        }
    }

    @ViewBuilder
    private var mascot: some View {
        if isOffline {
            // Observation failure ≠ zero activity: dim the mascot.
            ClaudeCrabIcon(size: 14, animateLegs: false)
                .opacity(0.35)
        } else {
            ClaudeCrabIcon(size: 14, animateLegs: claudeCount > 0)
                .offset(y: jumpOffset)
        }
    }

    /// Per-provider activity, or the offline chip. Beside a notch with no
    /// activity a quiet dot stands in so the right wing never collapses.
    @ViewBuilder
    private var rightWing: some View {
        HStack(spacing: 8) {
            activityItems
        }
    }

    /// Provider groups and the chip as separate views, so the notch layout
    /// can put each on either side of the notch.
    @ViewBuilder
    private var activityItems: some View {
        if isOffline {
            // Hide the (stale) session groups and SAY offline.
            chip("offline", color: Color.white.opacity(0.45))
        } else {
            if claudeCount > 0 {
                providerGroup(.claude, count: claudeCount, hue: claudeHue)
            }
            if codexCount > 0 {
                providerGroup(.codex, count: codexCount, hue: codexHue)
            }
            if grokCount > 0 {
                providerGroup(.grok, count: grokCount, hue: grokHue)
            }
            if geminiCount > 0 {
                providerGroup(.gemini, count: geminiCount, hue: geminiHue)
            }
            // No warning chip (AUTH 3, LIMIT 2, 7d 8%, degraded) on the
            // closed island: it made the label long, and the warnings
            // are in NEEDS ATTENTION when the island opens (owner
            // 2026-11-27). Only offline, above, replaces the counts.
            if notchGap != nil, claudeCount + codexCount + grokCount + geminiCount == 0 {
                Circle()
                    .fill(Color.white.opacity(0.35))
                    .frame(width: 5, height: 5)
            }
        }
    }

    /// The offline chip: a hairline divider + short text token.
    @ViewBuilder
    private func chip(_ text: String, color: Color) -> some View {
        HStack(spacing: 6) {
            Rectangle()
                .fill(Color.white.opacity(0.25))
                .frame(width: 1, height: 10)
            Text(text)
                .font(.system(size: 11, weight: .bold, design: .rounded))
                .monospacedDigit()
                .foregroundStyle(color)
                .lineLimit(1)
        }
    }

    @ViewBuilder
    private func providerGroup(_ provider: UsageProvider, count: Int, hue: Double) -> some View {
        HStack(spacing: 3) {
            UsageProviderIcon(provider: provider, size: 12)
                .hueRotation(.degrees(hue * 360))
            Text("\(count)")
                .font(.system(size: 11, weight: .bold, design: .rounded))
                .monospacedDigit()
                .foregroundStyle(Color(hue: hue, saturation: 0.85, brightness: 1.0))
        }
    }
}

/// The label's items (mascot, provider groups, chip) in order, split around
/// a gap of fixed width (a physical notch): the first items go left of the
/// gap, the rest right of it, at the split that makes the two wings closest
/// in width (`NotchSplit.best`). Both wings take the wider wing's width so
/// the gap stays centred. The items sit at the OUTER ends of their wings
/// (like the leading/trailing views of the iPhone Dynamic Island), so any
/// spare width lies next to the notch, where it reads as more notch, not as
/// an empty bar at the edge of the pill. A Layout rather than measured
/// @State, so the first frame (and a one-shot snapshot render) is already
/// correct.
struct NotchSplitLayout: Layout {
    let gap: CGFloat
    var spacing: CGFloat = 8

    private func split(_ subviews: Subviews) -> (widths: [CGFloat], left: Int, wing: CGFloat) {
        let widths = subviews.map { $0.sizeThatFits(.unspecified).width }
        let left = NotchSplit.best(widths: widths, spacing: spacing)
        let wing = max(NotchSplit.width(widths[..<left], spacing: spacing),
                       NotchSplit.width(widths[left...], spacing: spacing))
        return (widths, left, wing)
    }

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let height = subviews.map { $0.sizeThatFits(.unspecified).height }.max() ?? 0
        return CGSize(width: split(subviews).wing * 2 + gap, height: height)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        let (widths, left, _) = split(subviews)
        var x = bounds.minX
        for index in subviews.indices {
            if index == left { x = bounds.maxX - NotchSplit.width(widths[left...], spacing: spacing) }
            let size = subviews[index].sizeThatFits(.unspecified)
            subviews[index].place(at: CGPoint(x: x, y: bounds.midY), anchor: .leading, proposal: ProposedViewSize(size))
            x += widths[index] + spacing
        }
    }
}
