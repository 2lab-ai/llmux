import QtQuick
import QtQuick.Controls

ComboBox {
    id: control

    implicitHeight: 34
    leftPadding: 10
    rightPadding: 30
    font.pixelSize: 12
    palette.text: IslandTheme.primaryText
    palette.buttonText: IslandTheme.primaryText
    palette.highlight: IslandTheme.amberTint
    palette.highlightedText: IslandTheme.amber

    delegate: IslandItemDelegate {
        required property int index
        width: control.width
        text: control.textAt(index)
        highlighted: control.highlightedIndex === index
    }

    contentItem: Text {
        text: control.displayText
        color: control.enabled ? IslandTheme.primaryText : IslandTheme.tertiaryText
        font: control.font
        verticalAlignment: Text.AlignVCenter
        elide: Text.ElideRight
    }

    indicator: Text {
        x: control.width - width - 10
        anchors.verticalCenter: parent.verticalCenter
        text: "⌄"
        color: IslandTheme.secondaryText
        font.pixelSize: 14
    }

    background: Rectangle {
        radius: IslandTheme.controlRadius
        color: control.down ? IslandTheme.surfaceRaised : IslandTheme.field
        border.color: control.activeFocus ? IslandTheme.amber : IslandTheme.border
        border.width: 1
    }

    popup: Popup {
        y: control.height + 4
        width: control.width
        implicitHeight: contentItem.implicitHeight + 8
        padding: 4

        contentItem: ListView {
            clip: true
            implicitHeight: Math.min(contentHeight, 280)
            model: control.popup.visible ? control.delegateModel : null
            currentIndex: control.highlightedIndex
            ScrollIndicator.vertical: ScrollIndicator {}
        }

        background: Rectangle {
            color: IslandTheme.surfaceRaised
            radius: IslandTheme.controlRadius
            border.color: IslandTheme.borderStrong
        }
    }
}
