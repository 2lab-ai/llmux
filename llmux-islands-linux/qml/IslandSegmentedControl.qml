import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

Control {
    id: control

    property var model: []
    property int currentIndex: 0
    signal activated(int index)

    implicitHeight: 36
    padding: 3

    background: Rectangle {
        color: IslandTheme.surface
        radius: height / 2
        border.color: IslandTheme.border
        border.width: 1
    }

    contentItem: RowLayout {
        spacing: 3

        Repeater {
            model: control.model

            delegate: Button {
                id: segment
                required property int index
                required property var modelData
                Layout.fillWidth: true
                implicitHeight: 28
                checkable: true
                checked: index === control.currentIndex
                onClicked: control.activated(index)

                contentItem: Text {
                    text: String(segment.modelData).toUpperCase()
                    color: segment.checked ? IslandTheme.amber : IslandTheme.secondaryText
                    font.family: IslandTheme.monoFamily
                    font.pixelSize: 10
                    font.weight: Font.DemiBold
                    font.letterSpacing: 0.8
                    horizontalAlignment: Text.AlignHCenter
                    verticalAlignment: Text.AlignVCenter
                }

                background: Rectangle {
                    radius: height / 2
                    color: segment.checked ? IslandTheme.amberTint : "transparent"
                }
            }
        }
    }
}
