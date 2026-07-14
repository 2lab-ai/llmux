import QtQuick
import QtQuick.Controls

Switch {
    id: control

    spacing: 9
    palette.windowText: IslandTheme.primaryText

    indicator: Rectangle {
        implicitWidth: 34
        implicitHeight: 18
        x: control.leftPadding
        y: parent.height / 2 - height / 2
        radius: height / 2
        color: control.checked ? IslandTheme.greenTint : IslandTheme.surfaceRaised
        border.color: control.checked ? IslandTheme.green : IslandTheme.borderStrong

        Rectangle {
            width: 12
            height: 12
            radius: 6
            y: 3
            x: control.checked ? parent.width - width - 3 : 3
            color: control.checked ? IslandTheme.green : IslandTheme.secondaryText
            Behavior on x { NumberAnimation { duration: 120 } }
        }
    }

    contentItem: Label {
        text: control.text
        color: control.enabled ? IslandTheme.primaryText : IslandTheme.tertiaryText
        font.pixelSize: 12
        verticalAlignment: Text.AlignVCenter
        leftPadding: control.indicator.width + control.spacing
    }
}
