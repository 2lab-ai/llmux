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
    /// exception-beacon: the worst live exception, or `.none` for today's
    /// label pixel-identical. The chip is STATIC (decoupled from the rainbow
    /// loop) so a warning never reads as decoration.
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
    /// exception-beacon chip; `.none` renders today's label pixel-identical
    /// (no reserved slot, no spacer).
    var signal: GlanceSignal = .none
    /// Width of a physical notch the label must not draw under, or nil on a
    /// display without one. With a notch the label splits into two wings —
    /// the mascot left of the notch, activity and the exception chip right of
    /// it — so nothing hides behind the hardware.
    var notchGap: CGFloat? = nil

    var body: some View {
        if let notchGap {
            // Equal wings keep the notch centred in the pill.
            NotchSplitLayout(gap: notchGap + 16) {
                leftWing
                rightWing
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

            if isOffline {
                // Observation failure ≠ zero activity: dim the mascot.
                ClaudeCrabIcon(size: 14, animateLegs: false)
                    .opacity(0.35)
            } else {
                ClaudeCrabIcon(size: 14, animateLegs: claudeCount > 0)
                    .offset(y: jumpOffset)
            }
        }
    }

    /// Per-provider activity + the one exception chip. With no activity and
    /// no exception it is empty (the notch-split layout then shows a quiet dot
    /// so the right wing never collapses to nothing).
    @ViewBuilder
    private var rightWing: some View {
        HStack(spacing: 8) {
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
                if let text = signal.chipText {
                    chip(text, color: chipColor)
                } else if notchGap != nil, claudeCount + codexCount + grokCount + geminiCount == 0 {
                    Circle()
                        .fill(Color.white.opacity(0.35))
                        .frame(width: 5, height: 5)
                }
            }
        }
    }

    /// Severity color — reinforcement only; the TEXT carries the meaning.
    private var chipColor: Color {
        switch signal {
        case .auth, .limit: return Color(red: 1.0, green: 0.42, blue: 0.36)
        case .lowQuota: return Color(red: 1.0, green: 0.72, blue: 0.28)
        case .degraded: return Color(red: 0.95, green: 0.62, blue: 0.45)
        case .offline, .none: return Color.white.opacity(0.45)
        }
    }

    /// The one static exception chip: a hairline divider + short text token.
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

/// Two wings around a gap of fixed width (a physical notch): both wings take
/// the wider wing's natural width so the gap stays centred; the left wing's
/// content hugs the gap from the left, the right wing's from the right.
/// A Layout rather than measured @State, so the first frame (and a one-shot
/// snapshot render) is already correct.
struct NotchSplitLayout: Layout {
    let gap: CGFloat

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let sizes = subviews.map { $0.sizeThatFits(.unspecified) }
        let wing = sizes.map(\.width).max() ?? 0
        let height = sizes.map(\.height).max() ?? 0
        return CGSize(width: wing * 2 + gap, height: height)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        guard subviews.count == 2 else { return }
        let left = subviews[0].sizeThatFits(.unspecified)
        let right = subviews[1].sizeThatFits(.unspecified)
        let wing = max(left.width, right.width)
        subviews[0].place(
            at: CGPoint(x: bounds.minX + wing - left.width, y: bounds.midY),
            anchor: .leading,
            proposal: ProposedViewSize(left)
        )
        subviews[1].place(
            at: CGPoint(x: bounds.minX + wing + gap, y: bounds.midY),
            anchor: .leading,
            proposal: ProposedViewSize(right)
        )
    }
}
