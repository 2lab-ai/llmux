import SwiftUI

enum UsageDurationText {
    /// Creates a colorized duration label like `1d 10h 50m` or (when <1h) `12m 34s`.
    ///
    /// - Note: We keep digits and unit letters separately so we can color just the unit suffix.
    static func make(
        seconds: Int,
        digitColor: Color = Color.white.opacity(0.32),
        dayUnitColor: Color = TerminalColors.amber.opacity(0.95),
        hourUnitColor: Color = TerminalColors.blue.opacity(0.85),
        minuteUnitColor: Color = TerminalColors.cyan.opacity(0.55),
        secondUnitColor: Color = Color.white.opacity(0.35)
    ) -> Text {
        let clamped = max(0, seconds)
        let largestDigitSize: CGFloat = 13
        let mediumDigitSize: CGFloat = 11
        let baseDigitSize: CGFloat = 10

        func piece(_ value: String, size: CGFloat, color: Color) -> Text {
            Text(value)
                .font(.system(size: size, weight: .semibold, design: .monospaced))
                .foregroundColor(color)
        }

        func part(_ value: String, unit: String, unitColor: Color, digitSize: CGFloat, unitSize: CGFloat? = nil) -> Text {
            piece(value, size: digitSize, color: digitColor)
                + piece(unit, size: unitSize ?? max(9, digitSize - 1), color: unitColor)
        }

        let spacer = piece(" ", size: baseDigitSize, color: digitColor)

        if clamped < 60 {
            return part("<1", unit: "m", unitColor: minuteUnitColor, digitSize: largestDigitSize)
        }

        if clamped < 3_600 {
            let minutes = clamped / 60
            let seconds = clamped % 60
            return part("\(minutes)", unit: "m", unitColor: minuteUnitColor, digitSize: largestDigitSize)
                + spacer
                + part(String(format: "%02d", seconds), unit: "s", unitColor: secondUnitColor, digitSize: mediumDigitSize)
        }

        var remaining = clamped
        let days = remaining / 86_400
        remaining %= 86_400
        let hours = remaining / 3_600
        remaining %= 3_600
        let minutes = remaining / 60

        if days > 0 {
            return part("\(days)", unit: "d", unitColor: dayUnitColor, digitSize: largestDigitSize)
                + spacer
                + part("\(hours)", unit: "h", unitColor: hourUnitColor, digitSize: mediumDigitSize)
                + spacer
                + part("\(minutes)", unit: "m", unitColor: minuteUnitColor, digitSize: baseDigitSize)
        }

        return part("\(hours)", unit: "h", unitColor: hourUnitColor, digitSize: largestDigitSize)
            + spacer
            + part(String(format: "%02d", minutes), unit: "m", unitColor: minuteUnitColor, digitSize: baseDigitSize)
    }
}
