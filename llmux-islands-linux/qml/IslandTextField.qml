import QtQuick
import QtQuick.Controls

TextField {
    id: control

    implicitHeight: 34
    leftPadding: 10
    rightPadding: 10
    color: IslandTheme.primaryText
    selectionColor: IslandTheme.amber
    selectedTextColor: IslandTheme.panel
    placeholderTextColor: IslandTheme.tertiaryText
    font.pixelSize: 12
    palette.text: IslandTheme.primaryText

    background: Rectangle {
        radius: IslandTheme.controlRadius
        color: IslandTheme.field
        border.color: control.activeFocus ? IslandTheme.amber : IslandTheme.border
        border.width: 1
    }
}
