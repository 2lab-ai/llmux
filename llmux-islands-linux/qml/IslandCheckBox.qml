import QtQuick
import QtQuick.Controls

CheckBox {
    id: control

    spacing: 8

    indicator: Rectangle {
        implicitWidth: 18
        implicitHeight: 18
        x: control.leftPadding
        y: parent.height / 2 - height / 2
        radius: 0
        color: control.checked ? IslandTheme.primaryText : IslandTheme.field
        border.color: control.activeFocus
            ? (control.checked ? IslandTheme.panel : IslandTheme.focus)
            : IslandTheme.borderStrong
        border.width: control.activeFocus ? 2 : 1

        Text {
            anchors.centerIn: parent
            visible: control.checked
            text: "✓"
            color: IslandTheme.panel
            font.pixelSize: 12
            font.bold: true
        }

        Rectangle {
            anchors.fill: parent
            anchors.margins: -3
            visible: control.activeFocus
            color: "transparent"
            border.color: IslandTheme.focus
            border.width: 2
            radius: 0
        }
    }

    contentItem: Label {
        text: control.text
        color: control.enabled ? IslandTheme.primaryText : IslandTheme.disabledText
        font.pixelSize: 12
        verticalAlignment: Text.AlignVCenter
        leftPadding: control.indicator.width + control.spacing
    }
}
