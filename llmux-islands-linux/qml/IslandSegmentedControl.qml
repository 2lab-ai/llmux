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
        radius: 0
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
                    text: String(segment.modelData)
                    color: segment.checked ? IslandTheme.panel : IslandTheme.secondaryText
                    font.pixelSize: 11
                    font.weight: Font.DemiBold
                    horizontalAlignment: Text.AlignHCenter
                    verticalAlignment: Text.AlignVCenter
                }

                background: Rectangle {
                    radius: 0
                    color: segment.checked ? IslandTheme.primaryText : "transparent"
                    border.color: segment.visualFocus
                        ? (segment.checked ? IslandTheme.panel : IslandTheme.focus)
                        : segment.checked ? IslandTheme.primaryText : "transparent"
                    border.width: segment.visualFocus ? 2 : 1

                    Rectangle {
                        anchors.fill: parent
                        anchors.margins: -3
                        visible: segment.visualFocus
                        color: "transparent"
                        border.color: IslandTheme.focus
                        border.width: 2
                        radius: 0
                    }
                }
            }
        }
    }
}
