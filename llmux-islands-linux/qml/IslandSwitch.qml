import QtQuick
import QtQuick.Controls

Switch {
    id: control

    implicitHeight: IslandTheme.controlHeight
    leftPadding: 0
    rightPadding: 0
    topPadding: 0
    bottomPadding: 0
    spacing: IslandTheme.spaceSm
    palette.windowText: IslandTheme.primaryText

    indicator: Rectangle {
        implicitWidth: 34
        implicitHeight: 18
        x: 0
        y: parent.height / 2 - height / 2
        radius: height / 2
        color: control.checked ? IslandTheme.primaryText : IslandTheme.field
        border.color: control.activeFocus
            ? (control.checked ? IslandTheme.panel : IslandTheme.focus)
            : IslandTheme.borderStrong
        border.width: control.activeFocus ? 2 : 1

        Rectangle {
            width: 12
            height: 12
            radius: 6
            y: 3
            x: control.checked ? parent.width - width - 3 : 3
            color: control.checked ? IslandTheme.panel : IslandTheme.secondaryText
        }

        Rectangle {
            anchors.fill: parent
            anchors.margins: -3
            visible: control.activeFocus
            color: "transparent"
            border.color: IslandTheme.focus
            border.width: 2
            radius: height / 2
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
