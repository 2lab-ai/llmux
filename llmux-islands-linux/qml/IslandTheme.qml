pragma Singleton

import QtQuick

QtObject {
    readonly property color panel: "#050505"
    readonly property color surface: "#111111"
    readonly property color surfaceHover: "#171717"
    readonly property color surfaceRaised: "#1d1d1d"
    readonly property color field: "#151515"
    readonly property color border: "#292929"
    readonly property color borderStrong: "#3a3a3a"

    readonly property color primaryText: "#e6e6e6"
    readonly property color secondaryText: "#8a8a8a"
    readonly property color tertiaryText: "#5f5f5f"

    readonly property color green: "#66bf73"
    readonly property color amber: "#ffb300"
    readonly property color red: "#ff4d4d"
    readonly property color blue: "#6699ff"
    readonly property color magenta: "#cc66cc"
    readonly property color cyan: "#47b7b0"

    readonly property color greenTint: "#17331e"
    readonly property color amberTint: "#3a2903"
    readonly property color redTint: "#391417"
    readonly property color blueTint: "#17243d"

    readonly property int pagePadding: 18
    readonly property int sectionSpacing: 18
    readonly property int cardPadding: 12
    readonly property int cardRadius: 10
    readonly property int controlRadius: 8
    readonly property string monoFamily: "monospace"

    function providerAccent(provider) {
        switch (String(provider).toLowerCase()) {
        case "claude": return amber
        case "codex": return blue
        case "grok": return primaryText
        case "api": return magenta
        default: return secondaryText
        }
    }

    function quotaAccent(remaining, constraining, warningLevel) {
        if (constraining === true || warningLevel === "critical" || remaining <= 0.1)
            return red
        if (warningLevel === "warning" || remaining <= 0.35)
            return amber
        return green
    }
}
