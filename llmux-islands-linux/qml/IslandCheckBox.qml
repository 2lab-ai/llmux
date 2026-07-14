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
        radius: 5
        color: control.checked ? IslandTheme.amberTint : IslandTheme.field
        border.color: control.checked ? IslandTheme.amber : IslandTheme.borderStrong

        Text {
            anchors.centerIn: parent
            visible: control.checked
            text: "✓"
            color: IslandTheme.amber
            font.pixelSize: 12
            font.bold: true
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
