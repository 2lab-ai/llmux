import QtQuick
import QtQuick.Shapes

// Port of the macOS NotchShape (llmux-islands/.../NotchShape.swift): a flat
// top whose two upper corners curve INWARD (topRadius) and two rounded
// bottom corners (bottomRadius), filled black. Same quadratic curves and
// control points, so the outline matches the macOS island.
Shape {
    id: shape
    property real topRadius: 6
    property real bottomRadius: 14
    property color fillColor: "#000000"

    // Default renderer: it also draws under the software scene graph the
    // snapshot mode uses (CurveRenderer needs a GPU backend).
    antialiasing: true

    ShapePath {
        fillColor: shape.fillColor
        strokeWidth: -1
        startX: 0
        startY: 0
        PathQuad { x: shape.topRadius; y: shape.topRadius; controlX: shape.topRadius; controlY: 0 }
        PathLine { x: shape.topRadius; y: shape.height - shape.bottomRadius }
        PathQuad {
            x: shape.topRadius + shape.bottomRadius; y: shape.height
            controlX: shape.topRadius; controlY: shape.height
        }
        PathLine { x: shape.width - shape.topRadius - shape.bottomRadius; y: shape.height }
        PathQuad {
            x: shape.width - shape.topRadius; y: shape.height - shape.bottomRadius
            controlX: shape.width - shape.topRadius; controlY: shape.height
        }
        PathLine { x: shape.width - shape.topRadius; y: shape.topRadius }
        PathQuad { x: shape.width; y: 0; controlX: shape.width - shape.topRadius; controlY: 0 }
        PathLine { x: 0; y: 0 }
    }
}
