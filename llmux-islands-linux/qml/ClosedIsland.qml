import QtQuick
import QtQuick.Layouts

// Port of the macOS closed island (NotchClosedLabelView + the closed frame
// in NotchView / ClosedIslandSnapshotView): a black NotchShape, 38 high,
// 14 of horizontal padding, content at least 204 wide, centred:
//   "Llmux Islands" · mascot · per-provider icon + count (colour loop) · chip
// With a physical notch (notchGap >= 0) the content splits into equal
// wings around the notch, mascot left, activity and chip right, a quiet dot
// when idle — the same rule as the macOS NotchSplitLayout.
Item {
    id: island
    property int claudeCount: 0
    property int codexCount: 0
    property int grokCount: 0
    property int geminiCount: 0
    property bool offline: false
    property real notchGap: -1
    /// 0..1 position in the 3 s colour loop; animated while there is activity.
    property real hueLoopPhase: 0
    property bool active: true
    property string fontFamily: "Inter"

    readonly property int totalCount: claudeCount + codexCount + grokCount + geminiCount
    readonly property bool hasNotch: notchGap >= 0
    readonly property real closedHeight: 38
    readonly property real minContentWidth: 224 - 20
    readonly property real horizontalPadding: 14

    implicitHeight: closedHeight
    implicitWidth: (hasNotch ? split.implicitWidth : Math.max(minContentWidth, flat.implicitWidth))
        + horizontalPadding * 2

    NumberAnimation on hueLoopPhase {
        running: island.active && island.totalCount > 0
        from: 0; to: 1; duration: 3000; loops: Animation.Infinite
    }

    // The mascot hops while Claude sessions run: 1.2 s per hop at one
    // session, linearly faster to 0.25 s at ten, 60 % of each cycle in the
    // air, 3 px high (NotchClosedLabelView.jumpOffset on macOS).
    property real jumpPhase: 0
    readonly property real jumpPeriodMs: 1000 * (1.2 - (1.2 - 0.25) * (Math.min(Math.max(claudeCount, 1), 10) - 1) / 9)
    readonly property real jumpOffset: claudeCount < 1 || jumpPhase >= 0.6
        ? 0 : -3 * Math.sin(Math.PI * jumpPhase / 0.6)

    NumberAnimation on jumpPhase {
        running: island.active && !island.offline && island.claudeCount > 0
        from: 0; to: 1; duration: island.jumpPeriodMs; loops: Animation.Infinite
    }

    function rainbowHue(seed) {
        var hue = (hueLoopPhase + seed) % 1
        return hue < 0 ? hue + 1 : hue
    }

    NotchShape {
        anchors.fill: parent
        topRadius: 6
        bottomRadius: 14
    }

    // Inline components cannot see this file's ids, so each takes the island
    // as `isl`.
    // Advances of the label's fixed strings in SF Pro Rounded 11 pt on
    // macOS (semibold title, bold monospaced-digit chip), measured with
    // NSString.size(withAttributes:). Inter is spaced to these widths so
    // every element after a string lands where it does on macOS.
    readonly property var macAdvance: ({
        "Llmux Islands": 73.393,
        "offline": 35.777,
        "attention": 50.400
    })
    /// One monospaced digit of SF Pro Rounded Bold 11 pt.
    readonly property real macDigitAdvance: 7.675

    component MacText: Text {
        id: mt
        property real macWidth: -1
        TextMetrics {
            id: natural
            font.family: mt.font.family
            font.pixelSize: mt.font.pixelSize
            font.weight: mt.font.weight
            font.features: mt.font.features
            text: mt.text
        }
        font.letterSpacing: macWidth > 0 && text.length > 0
            ? (macWidth - natural.advanceWidth) / text.length : 0
    }

    component Title: MacText {
        property var isl
        text: "Llmux Islands"
        macWidth: isl.macAdvance[text] ?? -1
        color: Qt.rgba(1, 1, 1, 0.85)
        font.family: isl.fontFamily
        font.pixelSize: 11
        // macOS draws SF Rounded Semibold with CoreText's heavier
        // rasterization; Inter ExtraBold matches its visual weight here.
        font.weight: Font.ExtraBold
    }

    component ProviderGroup: Row {
        id: group
        property var isl
        property string iconSource: ""
        property string letter: ""
        property int count: 0
        property real seed: 0
        spacing: 3
        Item {
            width: 12; height: 12
            anchors.verticalCenter: parent.verticalCenter
            Image {
                anchors.fill: parent
                visible: group.iconSource !== ""
                source: group.iconSource
                sourceSize: Qt.size(24, 24)
                fillMode: Image.PreserveAspectFit
                smooth: true
            }
            Rectangle {
                anchors.fill: parent
                visible: group.iconSource === ""
                radius: 12 * 0.28
                color: Qt.rgba(1, 1, 1, 0.08)
                Text {
                    anchors.centerIn: parent
                    text: group.letter
                    color: Qt.rgba(1, 1, 1, 0.75)
                    font.family: group.isl.fontFamily
                    font.pixelSize: 12 * 0.72
                    font.weight: Font.DemiBold
                }
            }
        }
        Text {
            anchors.verticalCenter: parent.verticalCenter
            text: String(group.count)
            // Tabular digits on the macOS digit advance.
            width: text.length * group.isl.macDigitAdvance
            color: Qt.hsva(group.isl.rainbowHue(group.seed), 0.85, 1.0, 1)
            font.family: group.isl.fontFamily
            font.pixelSize: 11
            font.weight: Font.Bold
            font.features: { "tnum": 1 }
        }
    }

    component Chip: Row {
        id: chip
        property var isl
        property string label: ""
        property color tint: Qt.rgba(1, 1, 1, 0.45)
        spacing: 6
        Rectangle {
            width: 1; height: 10
            anchors.verticalCenter: parent.verticalCenter
            color: Qt.rgba(1, 1, 1, 0.25)
        }
        MacText {
            anchors.verticalCenter: parent.verticalCenter
            text: chip.label
            macWidth: chip.isl.macAdvance[text] ?? -1
            color: chip.tint
            font.family: chip.isl.fontFamily
            font.pixelSize: 11
            font.weight: Font.Bold
        }
    }

    component LeftWing: Row {
        id: lw
        property var isl
        property bool withTitle: true
        spacing: 8
        Title {
            isl: lw.isl
            visible: lw.withTitle
            anchors.verticalCenter: parent.verticalCenter
        }
        Item {
            implicitWidth: crab.implicitWidth
            implicitHeight: crab.implicitHeight
            anchors.verticalCenter: parent.verticalCenter
            CrabIcon {
                id: crab
                size: 14
                y: lw.isl.offline ? 0 : lw.isl.jumpOffset
                opacity: lw.isl.offline ? 0.35 : 1
                animateLegs: !lw.isl.offline && lw.isl.claudeCount > 0
                frozen: !lw.isl.active
            }
        }
    }

    component RightWing: Row {
        id: rw
        property var isl
        spacing: 8
        ProviderGroup {
            isl: rw.isl
            visible: !rw.isl.offline && count > 0
            iconSource: "qrc:/providers/claude.svg"; count: rw.isl.claudeCount; seed: 0
            anchors.verticalCenter: parent.verticalCenter
        }
        ProviderGroup {
            isl: rw.isl
            visible: !rw.isl.offline && count > 0
            iconSource: "qrc:/providers/codex.svg"; count: rw.isl.codexCount; seed: 0.35
            anchors.verticalCenter: parent.verticalCenter
        }
        ProviderGroup {
            isl: rw.isl
            visible: !rw.isl.offline && count > 0
            letter: "G"; count: rw.isl.grokCount; seed: 0.7
            anchors.verticalCenter: parent.verticalCenter
        }
        ProviderGroup {
            isl: rw.isl
            visible: !rw.isl.offline && count > 0
            iconSource: "qrc:/providers/gemini.png"; count: rw.isl.geminiCount; seed: 0.525
            anchors.verticalCenter: parent.verticalCenter
        }
        // Offline hides the (stale) provider groups and says so. No warning
        // chip on the closed island (macOS 2026-11-27): warnings live in the
        // opened panel.
        Chip {
            isl: rw.isl
            visible: rw.isl.offline
            label: "offline"
            anchors.verticalCenter: parent.verticalCenter
        }
    }

    // Display without a notch: one centred row.
    Row {
        id: flat
        visible: !island.hasNotch
        anchors.centerIn: parent
        spacing: 8
        LeftWing { isl: island; anchors.verticalCenter: parent.verticalCenter }
        RightWing { isl: island; anchors.verticalCenter: parent.verticalCenter }
    }

    // Display with a notch: the items (crab, provider groups, offline chip
    // or a quiet dot) split around the gap where the two wings come out
    // closest in width, each at the OUTER end of its wing, so spare width
    // sits next to the notch (NotchSplitLayout / NotchSplit on macOS).
    Item {
        id: split
        visible: island.hasNotch
        readonly property real gap: island.notchGap + 16
        readonly property real spacing: 8
        readonly property var items: [notchCrab, notchClaude, notchCodex, notchGrok, notchGemini,
                                      notchOffline, notchDot].filter(item => item.visible)
        readonly property var widths: items.map(item => item.implicitWidth)
        readonly property int leftCount: bestSplit(widths)
        readonly property real rightWidth: runWidth(widths.slice(leftCount))
        readonly property real wing: Math.max(runWidth(widths.slice(0, leftCount)), rightWidth)

        function runWidth(list) {
            return list.length === 0 ? 0 : list.reduce((a, b) => a + b, 0) + spacing * (list.length - 1)
        }
        // How many leading items go left: at least the crab, at most all but
        // one; the split whose wider wing is narrowest.
        function bestSplit(list) {
            if (list.length < 2)
                return list.length
            var best = 1, bestWing = Infinity
            for (var left = 1; left < list.length; ++left) {
                var w = Math.max(runWidth(list.slice(0, left)), runWidth(list.slice(left)))
                if (w < bestWing - 0.001) { bestWing = w; best = left }
            }
            return best
        }
        function xOf(item) {
            var index = items.indexOf(item)
            if (index < 0)
                return 0
            if (index < leftCount)
                return runWidth(widths.slice(0, index)) + (index > 0 ? spacing : 0)
            var start = 2 * wing + gap - rightWidth
            var before = widths.slice(leftCount, index)
            return start + runWidth(before) + (before.length > 0 ? spacing : 0)
        }

        implicitWidth: wing * 2 + gap
        implicitHeight: island.closedHeight
        anchors.centerIn: parent
        width: implicitWidth
        height: implicitHeight

        Item {
            id: notchCrab
            implicitWidth: notchCrabIcon.implicitWidth
            implicitHeight: notchCrabIcon.implicitHeight
            x: split.xOf(notchCrab)
            anchors.verticalCenter: parent.verticalCenter
            CrabIcon {
                id: notchCrabIcon
                size: 14
                y: island.offline ? 0 : island.jumpOffset
                opacity: island.offline ? 0.35 : 1
                animateLegs: !island.offline && island.claudeCount > 0
                frozen: !island.active
            }
        }
        ProviderGroup {
            id: notchClaude
            isl: island
            visible: !island.offline && count > 0
            iconSource: "qrc:/providers/claude.svg"; count: island.claudeCount; seed: 0
            x: split.xOf(notchClaude)
            anchors.verticalCenter: parent.verticalCenter
        }
        ProviderGroup {
            id: notchCodex
            isl: island
            visible: !island.offline && count > 0
            iconSource: "qrc:/providers/codex.svg"; count: island.codexCount; seed: 0.35
            x: split.xOf(notchCodex)
            anchors.verticalCenter: parent.verticalCenter
        }
        ProviderGroup {
            id: notchGrok
            isl: island
            visible: !island.offline && count > 0
            letter: "G"; count: island.grokCount; seed: 0.7
            x: split.xOf(notchGrok)
            anchors.verticalCenter: parent.verticalCenter
        }
        ProviderGroup {
            id: notchGemini
            isl: island
            visible: !island.offline && count > 0
            iconSource: "qrc:/providers/gemini.png"; count: island.geminiCount; seed: 0.525
            x: split.xOf(notchGemini)
            anchors.verticalCenter: parent.verticalCenter
        }
        Chip {
            id: notchOffline
            isl: island
            visible: island.offline
            label: "offline"
            x: split.xOf(notchOffline)
            anchors.verticalCenter: parent.verticalCenter
        }
        Rectangle {
            id: notchDot
            visible: !island.offline && island.totalCount === 0
            implicitWidth: 5; implicitHeight: 5
            width: 5; height: 5; radius: 2.5
            color: Qt.rgba(1, 1, 1, 0.35)
            x: split.xOf(notchDot)
            anchors.verticalCenter: parent.verticalCenter
        }
    }
}
