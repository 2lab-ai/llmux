import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

Button {
    id: control

    property color accentColor: IslandTheme.primaryText
    property bool destructive: false

    implicitHeight: 32
    leftPadding: display === AbstractButton.IconOnly ? 8 : 12
    rightPadding: leftPadding
    topPadding: 7
    bottomPadding: 7
    hoverEnabled: true
    opacity: enabled ? 1 : 0.44
    palette.buttonText: IslandTheme.primaryText
    palette.brightText: IslandTheme.primaryText

    contentItem: RowLayout {
        spacing: 6

        Kirigami.Icon {
            visible: control.display !== AbstractButton.TextOnly
                    && (control.icon.name.length > 0 || control.icon.source.toString().length > 0)
            source: control.icon.source.toString().length > 0
                ? control.icon.source : control.icon.name
            implicitWidth: 14
            implicitHeight: 14
            color: !control.enabled ? IslandTheme.disabledText
                : control.destructive ? IslandTheme.red
                : control.highlighted || control.checked
                    ? IslandTheme.panel : IslandTheme.secondaryText
        }

        Label {
            visible: control.display !== AbstractButton.IconOnly
            text: control.text
            color: !control.enabled ? IslandTheme.disabledText
                : control.destructive ? IslandTheme.red
                : control.highlighted || control.checked
                    ? IslandTheme.panel : IslandTheme.primaryText
            font.pixelSize: 12
            font.weight: control.highlighted || control.checked
                ? Font.DemiBold : Font.Medium
            horizontalAlignment: Text.AlignHCenter
            verticalAlignment: Text.AlignVCenter
            elide: Text.ElideRight
            Layout.fillWidth: true
        }
    }

    background: Rectangle {
        radius: IslandTheme.controlRadius
        color: !control.enabled ? IslandTheme.surface
            : control.highlighted || control.checked
                ? (control.down ? IslandTheme.secondaryText : IslandTheme.primaryText)
            : control.down ? IslandTheme.surfaceRaised
                : control.hovered ? IslandTheme.surfaceHover : IslandTheme.surface
        border.color: control.visualFocus
            ? (control.highlighted || control.checked ? IslandTheme.panel : IslandTheme.focus)
            : control.highlighted || control.checked
                ? IslandTheme.primaryText : IslandTheme.border
        border.width: control.visualFocus ? 2 : 1

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
