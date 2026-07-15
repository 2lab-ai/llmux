import QtQuick
import QtQuick.Controls

ProgressBar {
    id: control

    property color accentColor: IslandTheme.primaryText

    implicitHeight: 9

    background: Rectangle {
        implicitHeight: 9
        radius: 0
        color: IslandTheme.surfaceRaised
        border.color: IslandTheme.border
        border.width: 1
    }

    contentItem: Item {
        clip: true

        Rectangle {
            width: control.indeterminate ? 0 : control.visualPosition * parent.width
            height: parent.height
            radius: 0
            color: control.accentColor
        }
    }
}
