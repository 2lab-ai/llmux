import QtQuick
import QtQuick.Controls

TextArea {
    id: control

    leftPadding: 10
    rightPadding: 10
    topPadding: 9
    bottomPadding: 9
    color: IslandTheme.primaryText
    selectionColor: IslandTheme.amber
    selectedTextColor: IslandTheme.panel
    placeholderTextColor: IslandTheme.tertiaryText
    font.pixelSize: 12

    background: Rectangle {
        radius: IslandTheme.controlRadius
        color: IslandTheme.field
        border.color: control.activeFocus ? IslandTheme.amber : IslandTheme.border
        border.width: 1
    }
}
