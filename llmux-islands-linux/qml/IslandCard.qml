import QtQuick
import QtQuick.Controls

Control {
    id: control

    property color fillColor: hovered ? IslandTheme.surfaceHover : IslandTheme.surface
    property color strokeColor: IslandTheme.border
    property int cornerRadius: IslandTheme.cardRadius
    property bool interactive: false

    padding: IslandTheme.cardPadding
    hoverEnabled: interactive
    palette.text: IslandTheme.primaryText
    palette.windowText: IslandTheme.primaryText

    background: Rectangle {
        color: control.fillColor
        radius: control.cornerRadius
        border.color: control.strokeColor
        border.width: 1
    }
}
