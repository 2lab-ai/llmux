import Foundation

/// Where the notch splits the label's items. Pure, so it is tested.
enum NotchSplit {
    /// Width of a run of items laid out with `spacing` between them.
    static func width<C: Collection>(_ widths: C, spacing: CGFloat) -> CGFloat where C.Element == CGFloat {
        widths.isEmpty ? 0 : widths.reduce(0, +) + spacing * CGFloat(widths.count - 1)
    }

    /// How many leading items go left of the notch: at least the first (the
    /// mascot), at most all but one when there is more than one, choosing
    /// the split whose wider wing is narrowest. Ties keep more on the right.
    static func best(widths: [CGFloat], spacing: CGFloat) -> Int {
        guard widths.count > 1 else { return widths.count }
        var bestLeft = 1
        var bestWing = CGFloat.infinity
        for left in 1..<widths.count {
            let wing = max(width(widths[..<left], spacing: spacing), width(widths[left...], spacing: spacing))
            if wing < bestWing - 0.001 {
                bestWing = wing
                bestLeft = left
            }
        }
        return bestLeft
    }
}
