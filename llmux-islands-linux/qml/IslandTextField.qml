import QtQuick
import QtQuick.Controls

TextField {
    id: control

    implicitHeight: 34
    leftPadding: 10
    rightPadding: 10
    color: IslandTheme.primaryText
    selectionColor: IslandTheme.primaryText
    selectedTextColor: IslandTheme.panel
    placeholderTextColor: IslandTheme.tertiaryText
    font.pixelSize: 12
    palette.text: IslandTheme.primaryText

    background: Rectangle {
        radius: IslandTheme.controlRadius
        color: IslandTheme.field
        border.color: control.activeFocus ? IslandTheme.focus : IslandTheme.border
        border.width: control.activeFocus ? 2 : 1
    }
}
