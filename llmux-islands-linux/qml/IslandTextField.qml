import QtQuick
import QtQuick.Controls

TextField {
    id: control

    implicitHeight: IslandTheme.controlHeight
    leftPadding: IslandTheme.controlPaddingX
    rightPadding: IslandTheme.controlPaddingX
    topPadding: 0
    bottomPadding: 0
    verticalAlignment: TextInput.AlignVCenter
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
