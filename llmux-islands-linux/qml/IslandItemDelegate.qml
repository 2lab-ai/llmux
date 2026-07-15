import QtQuick
import QtQuick.Controls

ItemDelegate {
    id: control

    implicitHeight: IslandTheme.controlHeight
    leftPadding: IslandTheme.controlPaddingX
    rightPadding: IslandTheme.controlPaddingX
    topPadding: 0
    bottomPadding: 0
    hoverEnabled: true

    contentItem: Label {
        text: control.text
        color: control.highlighted ? IslandTheme.panel : IslandTheme.primaryText
        font.pixelSize: 12
        verticalAlignment: Text.AlignVCenter
        elide: Text.ElideRight
    }

    background: Rectangle {
        color: control.highlighted ? IslandTheme.primaryText
            : control.hovered ? IslandTheme.surfaceHover : "transparent"
        radius: 0
        border.color: control.visualFocus
            ? (control.highlighted ? IslandTheme.panel : IslandTheme.focus)
            : "transparent"
        border.width: control.visualFocus ? 2 : 0

        Rectangle {
            anchors.fill: parent
            anchors.margins: -3
            visible: control.visualFocus
            color: "transparent"
            border.color: IslandTheme.focus
            border.width: 2
            radius: 0
        }
    }
}
