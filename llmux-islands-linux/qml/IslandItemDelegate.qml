import QtQuick
import QtQuick.Controls

ItemDelegate {
    id: control

    implicitHeight: 32
    leftPadding: 10
    rightPadding: 10
    hoverEnabled: true

    contentItem: Label {
        text: control.text
        color: control.highlighted ? IslandTheme.amber : IslandTheme.primaryText
        font.pixelSize: 12
        verticalAlignment: Text.AlignVCenter
        elide: Text.ElideRight
    }

    background: Rectangle {
        color: control.highlighted ? IslandTheme.amberTint
            : control.hovered ? IslandTheme.surfaceHover : "transparent"
        radius: 6
    }
}
