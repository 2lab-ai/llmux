import QtQuick
import QtQuick.Controls
import org.kde.kirigami as Kirigami

Control {
    id: control

    property int type: Kirigami.MessageType.Information
    property string text: ""

    readonly property bool isError: type === Kirigami.MessageType.Error
    readonly property bool isWarning: type === Kirigami.MessageType.Warning
    readonly property color accentColor: isError ? IslandTheme.red
        : isWarning ? IslandTheme.amber : IslandTheme.blue

    padding: 10

    contentItem: Label {
        text: control.text
        color: IslandTheme.primaryText
        wrapMode: Text.Wrap
        font.pixelSize: 12
    }

    background: Rectangle {
        radius: IslandTheme.controlRadius
        color: control.isError ? IslandTheme.redTint
            : control.isWarning ? IslandTheme.amberTint : IslandTheme.blueTint
        border.color: control.accentColor
        border.width: 1
    }
}
