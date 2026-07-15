pragma Singleton

import QtQuick

QtObject {
    readonly property color panel: "#000000"
    readonly property color surface: "#000000"
    readonly property color surfaceHover: Qt.rgba(1, 1, 1, 0.04)
    readonly property color surfaceRaised: Qt.rgba(1, 1, 1, 0.08)
    readonly property color field: "#000000"
    readonly property color border: Qt.rgba(1, 1, 1, 0.12)
    readonly property color borderStrong: Qt.rgba(1, 1, 1, 0.28)

    readonly property color primaryText: "#ffffff"
    readonly property color secondaryText: Qt.rgba(1, 1, 1, 0.60)
    readonly property color tertiaryText: Qt.rgba(1, 1, 1, 0.50)
    readonly property color disabledText: Qt.rgba(1, 1, 1, 0.44)
    readonly property color focus: "#ffffff"

    readonly property color green: "#66bf73"
    readonly property color amber: "#ffb300"
    readonly property color red: "#ff4d4d"
    readonly property color greenTint: "#17331e"
    readonly property color amberTint: "#3a2903"
    readonly property color redTint: "#391417"

    readonly property int pagePadding: 24
    readonly property int sectionSpacing: 24
    readonly property int cardPadding: 16
    readonly property int cardRadius: 0
    readonly property int controlRadius: 0
    readonly property string monoFamily: "monospace"

    function providerAccent(provider) {
        // Provider identity is carried by copy and iconography. Keeping the
        // normal state neutral prevents branding color from competing with
        // warning and error signals.
        return secondaryText
    }

    function quotaAccent(remaining, constraining, warningLevel) {
        if (constraining === true || warningLevel === "critical" || remaining <= 0.1)
            return red
        if (warningLevel === "warning" || remaining <= 0.35)
            return amber
        return primaryText
    }
}
