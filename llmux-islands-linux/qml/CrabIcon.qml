import QtQuick
import QtQuick.Shapes

// Port of the macOS ClaudeCrabIcon (pixel crab, 66x52 viewBox): antennas,
// four legs that alternate while active, body, two eyes. Same rectangles,
// the same 0.15 s leg cycle, and drawn the same way: vector rectangles at
// fractional positions with antialiasing, so a 2x render is as sharp as
// the macOS Canvas. (Rectangle items snapped every leg and eye a pixel
// wider; a Canvas painted a 1x texture that blurred when scaled.)
Item {
    id: crab
    property real size: 14
    property color color: Qt.rgba(0.85, 0.47, 0.34, 1)
    property bool animateLegs: false
    property int legPhase: 0
    // Snapshot mode freezes the cycle so renders are comparable.
    property bool frozen: false
    readonly property real unit: size / 52

    implicitWidth: size * 66 / 52
    implicitHeight: size

    readonly property var legOffsets: [[3, -3, 3, -3], [0, 0, 0, 0], [-3, 3, -3, 3], [0, 0, 0, 0]]

    Timer {
        interval: 150
        repeat: true
        running: crab.animateLegs && !crab.frozen
        onTriggered: crab.legPhase = (crab.legPhase + 1) % 4
    }

    // Rectangles in viewBox units as an SVG path, scaled to `size`.
    function rects(list) {
        const u = unit
        return list.map(r => "M" + r[0] * u + " " + r[1] * u
                        + "H" + (r[0] + r[2]) * u + "V" + (r[1] + r[3]) * u
                        + "H" + r[0] * u + "Z").join(" ")
    }
    function leg(index, x) {
        return [x, 39, 6, 13 + (animateLegs ? legOffsets[legPhase % 4][index] : 0)]
    }

    // The Shape is a child, so the icon's size never depends on how the
    // Shape measures its paths.
    Shape {
        anchors.fill: parent
        antialiasing: true
        ShapePath {
            fillColor: crab.color
            strokeWidth: -1
            strokeColor: "transparent"
            PathSvg {
                path: crab.rects([[0, 13, 6, 13], [60, 13, 6, 13],
                                  crab.leg(0, 6), crab.leg(1, 18), crab.leg(2, 42), crab.leg(3, 54),
                                  [6, 0, 54, 39]])
            }
        }
        ShapePath {
            fillColor: "#000000"
            strokeWidth: -1
            strokeColor: "transparent"
            PathSvg { path: crab.rects([[12, 13, 6, 6.5], [48, 13, 6, 6.5]]) }
        }
    }
}
