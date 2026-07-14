import QtQuick
import QtQuick.Controls

Dialog {
    id: control

    padding: 16
    palette.window: IslandTheme.surface
    palette.windowText: IslandTheme.primaryText
    palette.text: IslandTheme.primaryText
    palette.button: IslandTheme.surfaceRaised
    palette.buttonText: IslandTheme.primaryText
    palette.base: IslandTheme.field
    palette.highlight: IslandTheme.amber
    palette.highlightedText: IslandTheme.panel

    background: Rectangle {
        color: IslandTheme.surface
        radius: 14
        border.color: IslandTheme.borderStrong
        border.width: 1
    }
}
