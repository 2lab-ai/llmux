import QtQuick
import QtQuick.Controls

TextArea {
    id: control

    leftPadding: IslandTheme.controlPaddingX
    rightPadding: IslandTheme.controlPaddingX
    topPadding: 9
    bottomPadding: 9
    color: IslandTheme.primaryText
    selectionColor: IslandTheme.primaryText
    selectedTextColor: IslandTheme.panel
    placeholderTextColor: IslandTheme.tertiaryText
    font.pixelSize: 12

    background: Rectangle {
        radius: IslandTheme.controlRadius
        color: IslandTheme.field
        border.color: control.activeFocus ? IslandTheme.focus : IslandTheme.border
        border.width: control.activeFocus ? 2 : 1
    }
}
