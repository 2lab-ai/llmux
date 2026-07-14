import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

Button {
    id: control

    property color accentColor: IslandTheme.amber
    property bool destructive: false

    implicitHeight: 32
    leftPadding: display === AbstractButton.IconOnly ? 8 : 12
    rightPadding: leftPadding
    topPadding: 7
    bottomPadding: 7
    hoverEnabled: true
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
            color: control.destructive ? IslandTheme.red
                : control.highlighted || control.checked
                    ? control.accentColor : IslandTheme.secondaryText
        }

        Label {
            visible: control.display !== AbstractButton.IconOnly
            text: control.text
            color: control.destructive ? IslandTheme.red
                : control.highlighted || control.checked
                    ? control.accentColor : IslandTheme.primaryText
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
            : control.down ? IslandTheme.surfaceRaised
            : control.highlighted || control.checked
                ? IslandTheme.amberTint
                : control.hovered ? IslandTheme.surfaceHover : IslandTheme.surface
        border.color: control.highlighted || control.checked
            ? Qt.rgba(1, 0.7, 0, 0.45) : IslandTheme.border
        border.width: 1
        opacity: control.enabled ? 1 : 0.45
    }
}
