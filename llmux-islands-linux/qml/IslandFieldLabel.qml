import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

Label {
    Layout.alignment: Qt.AlignRight | Qt.AlignVCenter
    Layout.preferredWidth: IslandTheme.fieldLabelWidth
    Layout.preferredHeight: IslandTheme.controlHeight
    color: IslandTheme.secondaryText
    font.pixelSize: 11
    font.weight: Font.Medium
    horizontalAlignment: Text.AlignRight
    verticalAlignment: Text.AlignVCenter
}
