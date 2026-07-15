pragma ComponentBehavior: Bound

import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

Kirigami.ScrollablePage {
    id: statisticsPage

    property var uiState: ({})
    property bool advancedVisible: false
    property bool receiptSnapshotMode: false
    property int selectedSection: 0
    property int renderedHeatmapCells: 0
    property int renderedServingAccounts: 0
    property alias snapshotReceiptTarget: receiptEvidenceSection
    readonly property var statistics: objectOrEmpty(uiState.statistics)
    readonly property var overview: objectOrEmpty(statistics.overview)
    readonly property var models: arrayOrEmpty(statistics.models)
    readonly property var clients: arrayOrEmpty(statistics.clients)
    readonly property var health: arrayOrEmpty(statistics.health)
    readonly property var heatmaps: arrayOrEmpty(statistics.heatmaps)
    readonly property var activityReceipts: arrayOrEmpty(statistics.activity_receipts)
    readonly property var verificationReceipts: arrayOrEmpty(uiState.verification_receipts)
    readonly property var dataQuality: objectOrEmpty(statistics.data_quality)
    readonly property bool effectiveAdvancedVisible: advancedVisible || receiptSnapshotMode
    readonly property string unavailableText: qsTr("Unavailable")
    readonly property real preferredContentHeight: statisticsContent.implicitHeight

    title: qsTr("Statistics")
    padding: IslandTheme.pagePadding
    palette.window: IslandTheme.panel
    palette.windowText: IslandTheme.primaryText
    palette.text: IslandTheme.primaryText
    palette.buttonText: IslandTheme.primaryText
    palette.base: IslandTheme.field
    palette.highlight: IslandTheme.primaryText
    palette.highlightedText: IslandTheme.panel
    background: Rectangle { color: IslandTheme.panel }

    function arrayLikeLength(value) {
        if (Array.isArray(value))
            return value.length
        if (value === null || value === undefined || typeof value !== "object")
            return -1
        var length = Number(value.length)
        return isFinite(length) && length >= 0 && Math.floor(length) === length
                ? length : -1
    }

    function arrayOrEmpty(value) {
        var length = arrayLikeLength(value)
        if (length < 0)
            return []
        if (Array.isArray(value))
            return value
        var result = []
        for (var index = 0; index < length; index += 1)
            result.push(value[index])
        return result
    }

    function objectOrEmpty(value) {
        return value !== null && value !== undefined
                && typeof value === "object" && arrayLikeLength(value) < 0 ? value : ({})
    }

    function hasValue(value) {
        return value !== null && value !== undefined && value !== ""
    }

    function hasOwn(value, key) {
        return value !== null && value !== undefined
                && typeof value === "object"
                && Object.prototype.hasOwnProperty.call(value, key)
    }

    function firstAvailable(value, keys) {
        var source = objectOrEmpty(value)
        for (var index = 0; index < keys.length; index += 1) {
            if (hasValue(source[keys[index]]))
                return source[keys[index]]
        }
        return null
    }

    function compactNumber(value) {
        if (!hasValue(value))
            return unavailableText
        var number = Number(value)
        if (!isFinite(number))
            return String(value)
        var absolute = Math.abs(number)
        if (absolute >= 1000000000)
            return (number / 1000000000).toFixed(1).replace(/\.0$/, "") + "B"
        if (absolute >= 1000000)
            return (number / 1000000).toFixed(1).replace(/\.0$/, "") + "M"
        if (absolute >= 1000)
            return (number / 1000).toFixed(1).replace(/\.0$/, "") + "K"
        return String(number)
    }

    function optionalNumber(value, suffix) {
        if (!hasValue(value))
            return unavailableText
        var rendered = compactNumber(value)
        return hasValue(suffix) ? rendered + suffix : rendered
    }

    function optionalCurrency(value) {
        if (!hasValue(value))
            return unavailableText
        var number = Number(value)
        return isFinite(number) ? "$" + number.toFixed(4) : unavailableText
    }

    function optionalTime(value) {
        if (!hasValue(value))
            return unavailableText
        var milliseconds = Number(value)
        if (!isFinite(milliseconds) || milliseconds < 0)
            return unavailableText
        var date = new Date(milliseconds)
        return isNaN(date.getTime()) ? unavailableText : Qt.formatDateTime(date, "yyyy-MM-dd hh:mm:ss")
    }

    function optionalDuration(value) {
        if (!hasValue(value))
            return unavailableText
        var milliseconds = Number(value)
        if (!isFinite(milliseconds) || milliseconds < 0)
            return unavailableText
        if (milliseconds < 1000)
            return Math.round(milliseconds) + " ms"
        if (milliseconds < 60000)
            return (milliseconds / 1000).toFixed(1).replace(/\.0$/, "") + " s"
        return Math.floor(milliseconds / 60000) + "m "
                + Math.floor((milliseconds % 60000) / 1000) + "s"
    }

    function optionalText(value) {
        return hasValue(value) ? String(value) : unavailableText
    }

    function totalTokensText(source) {
        var values = objectOrEmpty(source)
        if (hasValue(values.tokens))
            return optionalNumber(values.tokens)
        if (!hasValue(values.tokens_in) || !hasValue(values.tokens_out))
            return unavailableText
        var input = Number(values.tokens_in)
        var output = Number(values.tokens_out)
        return isFinite(input) && isFinite(output)
                ? compactNumber(input + output) : unavailableText
    }

    function errorRateText() {
        if (hasValue(overview.error_rate)) {
            var supplied = Number(overview.error_rate)
            if (isFinite(supplied))
                return (supplied <= 1 ? supplied * 100 : supplied).toFixed(1) + "%"
            return String(overview.error_rate)
        }
        if (!hasValue(overview.requests) || !hasValue(overview.errors))
            return unavailableText
        var requests = Number(overview.requests)
        var errors = Number(overview.errors)
        if (!isFinite(requests) || !isFinite(errors))
            return unavailableText
        if (requests === 0)
            return "0%"
        return (errors / requests * 100).toFixed(1) + "%"
    }

    function overviewTokensText() {
        return totalTokensText({
            "tokens": overview.tokens,
            "tokens_in": overview.tokens_in,
            "tokens_out": overview.tokens_out
        })
    }

    function accountCountText() {
        if (hasValue(overview.accounts)) {
            if (arrayLikeLength(overview.accounts) >= 0)
                return String(arrayOrEmpty(overview.accounts).length)
            return optionalNumber(overview.accounts)
        }
        return hasOwn(statistics, "health") ? String(health.length) : unavailableText
    }

    function attentionAccountCount() {
        var count = 0
        for (var index = 0; index < health.length; index += 1) {
            var account = objectOrEmpty(health[index])
            if (account.healthy === false || account.paused === true
                    || hasValue(account.blocked_reason))
                count += 1
        }
        return count
    }

    function renderedHeatmapCellCount() {
        return renderedHeatmapCells
    }

    function renderedServingAccountCount() {
        return renderedServingAccounts
    }

    function renderedVerificationReceiptCount() {
        return verificationReceipts.length
    }

    function verificationOutcomeColor(outcome) {
        switch (String(outcome)) {
        case "succeeded": return IslandTheme.green
        case "failed": return IslandTheme.red
        case "cancelled": return IslandTheme.amber
        default: return IslandTheme.secondaryText
        }
    }

    function qualityValue(key) {
        var value = dataQuality[key]
        if (!hasValue(value))
            return unavailableText
        if (typeof value === "object")
            return optionalText(firstAvailable(value, ["label", "message", "status", "quality"]))
        return String(value)
    }

    function abbreviatedClient(value) {
        if (!hasValue(value))
            return unavailableText
        var text = String(value)
        if (text.length <= 24)
            return text
        return text.slice(0, 15) + "…" + text.slice(-6)
    }

    function heatmapForWindow(windowName) {
        for (var index = 0; index < heatmaps.length; index += 1) {
            var candidate = objectOrEmpty(heatmaps[index])
            if (String(candidate.window).toLowerCase() === String(windowName).toLowerCase())
                return candidate
            var seconds = Number(candidate.window_secs)
            if (windowName === "24h" && seconds === 86400)
                return candidate
            if (windowName === "72h" && seconds === 259200)
                return candidate
        }
        return ({})
    }

    function heatCellIntensity(cell) {
        var value = firstAvailable(cell, ["tokens", "tokens_total"])
        if (!hasValue(value)) {
            if (hasValue(cell.tokens_in) && hasValue(cell.tokens_out))
                value = Number(cell.tokens_in) + Number(cell.tokens_out)
            else
                return 0.08
        }
        var tokens = Math.max(0, Number(value))
        if (!isFinite(tokens))
            return 0.08
        return Math.min(0.85, 0.12 + Math.log(tokens + 1) / 16)
    }

    function modelTitle(row) {
        var model = optionalText(row.model)
        return hasValue(row.group) ? String(row.group) + " · " + model : model
    }

    function healthCredential(row) {
        return optionalText(firstAvailable(row, ["credential_type", "kind", "type"]))
    }

    function healthCooldown(row) {
        var blocked = firstAvailable(row, ["blocked_reason", "blocked"])
        var until = firstAvailable(row, ["cooldown_until_ms", "cooldown_until"])
        var source = firstAvailable(row, ["cooldown_source", "block_source"])
        if (hasValue(blocked))
            return String(blocked)
        if (hasValue(until)) {
            var text = qsTr("until %1").arg(optionalTime(until))
            return hasValue(source) ? text + " · " + source : text
        }
        return qsTr("None")
    }

    function healthRefresh(row) {
        var explicit = firstAvailable(row, ["refresh_state", "refresh_status"])
        if (hasValue(explicit))
            return String(explicit)
        return optionalTime(firstAvailable(row, ["last_refresh_ms", "refreshed_at_ms"]))
    }

    function healthSummaryReason(row) {
        var account = objectOrEmpty(row)
        var blocked = firstAvailable(account, ["blocked_reason", "blocked"])
        if (hasValue(blocked))
            return String(blocked)
        if (account.healthy === false) {
            if (hasValue(account.status))
                return String(account.status)
            return qsTr("Account health or authentication needs attention")
        }
        if (account.paused === true)
            return qsTr("Paused by user")
        return ""
    }

    function receiptStatusText(receipt) {
        if (receipt.kind === "in_flight")
            return qsTr("In flight")
        if (receipt.kind === "note")
            return receipt.error ? qsTr("Error note") : qsTr("Note")
        return hasValue(receipt.status) ? String(receipt.status) : qsTr("Completed")
    }

    function receiptStatusColor(receipt) {
        if (receipt.kind === "in_flight")
            return IslandTheme.surfaceRaised
        if (receipt.error || (hasValue(receipt.status) && Number(receipt.status) >= 400))
            return IslandTheme.redTint
        if (receipt.kind === "note")
            return IslandTheme.surfaceRaised
        return IslandTheme.surfaceRaised
    }

    function receiptHeadline(receipt) {
        if (receipt.kind === "note")
            return optionalText(receipt.message)
        var method = optionalText(receipt.method)
        var path = optionalText(receipt.path)
        return method + " · " + path
    }

    function receiptTarget(receipt) {
        var segments = []
        if (hasValue(receipt.account_display))
            segments.push(String(receipt.account_display))
        if (hasValue(receipt.provider))
            segments.push(String(receipt.provider))
        if (hasValue(receipt.model))
            segments.push(String(receipt.model))
        return segments.length > 0 ? segments.join(" · ") : unavailableText
    }

    function receiptTokenInput(receipt) {
        var tokens = objectOrEmpty(receipt.tokens)
        return optionalNumber(tokens.input)
    }

    function receiptTokenOutput(receipt) {
        var tokens = objectOrEmpty(receipt.tokens)
        return optionalNumber(tokens.output)
    }

    function receiptCacheRead(receipt) {
        var cache = objectOrEmpty(receipt.cache)
        return optionalNumber(cache.read)
    }

    function receiptCacheCreation(receipt) {
        var cache = objectOrEmpty(receipt.cache)
        return optionalNumber(cache.creation)
    }

    function receiptTiming(receipt) {
        if (receipt.kind === "in_flight")
            return optionalDuration(receipt.elapsed_ms)
        return optionalDuration(receipt.duration_ms)
    }

    ColumnLayout {
        id: statisticsContent
        width: statisticsPage.availableWidth
        spacing: IslandTheme.sectionSpacing

        RowLayout {
            Layout.fillWidth: true

            Label {
                text: qsTr("Statistics")
                color: IslandTheme.primaryText
                font.pixelSize: 20
                font.weight: Font.DemiBold
            }

            Rectangle {
                radius: 0
                implicitWidth: statisticsConnectionLabel.implicitWidth + 18
                implicitHeight: statisticsConnectionLabel.implicitHeight + 10
                color: IslandTheme.surface
                border.color: IslandTheme.border
                border.width: 1

                Label {
                    id: statisticsConnectionLabel
                    anchors.centerIn: parent
                    text: qsTr("%1 accounts").arg(statisticsPage.accountCountText())
                    color: IslandTheme.secondaryText
                    font.pixelSize: 11
                }
            }

            Item { Layout.fillWidth: true }

            IslandButton {
                objectName: "statistics-advanced-disclosure"
                text: qsTr("Advanced")
                checkable: true
                checked: statisticsPage.effectiveAdvancedVisible
                enabled: !statisticsPage.receiptSnapshotMode
                Accessible.name: qsTr("Show advanced statistics details")
                onClicked: statisticsPage.advancedVisible = checked
            }
        }

        IslandSegmentedControl {
            Layout.fillWidth: true
            visible: statisticsPage.effectiveAdvancedVisible
            model: [qsTr("Models"), qsTr("Clients"), qsTr("Health"), qsTr("Receipts")]
            currentIndex: statisticsPage.receiptSnapshotMode
                ? 3 : statisticsPage.selectedSection
            onActivated: function(index) { statisticsPage.selectedSection = index }
        }

        IslandInlineMessage {
            Layout.fillWidth: true
            visible: statisticsPage.attentionAccountCount() > 0
            type: Kirigami.MessageType.Warning
            text: qsTr("%1 account needs attention")
                .arg(statisticsPage.attentionAccountCount())
        }

        ColumnLayout {
            objectName: "statistics-overview"
            Layout.fillWidth: true
            spacing: Kirigami.Units.smallSpacing

            RowLayout {
                Layout.fillWidth: true
                IslandSectionLabel {
                    text: qsTr("Overview")
                }
                Item { Layout.fillWidth: true }
                Label {
                    visible: statisticsPage.effectiveAdvancedVisible
                    text: qsTr("Model data: %1 · Cost: %2")
                            .arg(statisticsPage.qualityValue("model_usage"))
                            .arg(statisticsPage.qualityValue("cost"))
                    color: IslandTheme.secondaryText
                    font.family: IslandTheme.monoFamily
                    font.pixelSize: 9
                    elide: Text.ElideRight
                }
            }

            GridLayout {
                Layout.fillWidth: true
                columns: statisticsPage.width >= 760
                    ? (statisticsPage.effectiveAdvancedVisible ? 5 : 4) : 2
                columnSpacing: Kirigami.Units.smallSpacing
                rowSpacing: Kirigami.Units.smallSpacing

                Repeater {
                    model: statisticsPage.effectiveAdvancedVisible ? [
                        { "label": qsTr("Requests"), "value": statisticsPage.optionalNumber(statisticsPage.overview.requests) },
                        { "label": qsTr("Tokens"), "value": statisticsPage.overviewTokensText() },
                        { "label": qsTr("API-equivalent cost"), "value": statisticsPage.optionalCurrency(statisticsPage.overview.cost_usd) },
                        { "label": qsTr("Error rate"), "value": statisticsPage.errorRateText() },
                        { "label": qsTr("Accounts"), "value": statisticsPage.accountCountText() }
                    ] : [
                        { "label": qsTr("Requests"), "value": statisticsPage.optionalNumber(statisticsPage.overview.requests) },
                        { "label": qsTr("Tokens"), "value": statisticsPage.overviewTokensText() },
                        { "label": qsTr("Error rate"), "value": statisticsPage.errorRateText() },
                        { "label": qsTr("Accounts"), "value": statisticsPage.accountCountText() }
                    ]

                    delegate: IslandCard {
                        id: overviewCard
                        required property var modelData
                        Layout.fillWidth: true

                        contentItem: ColumnLayout {
                            spacing: 4
                            IslandSectionLabel {
                                text: overviewCard.modelData.label
                            }
                            Label {
                                text: overviewCard.modelData.value
                                color: IslandTheme.primaryText
                                font.family: IslandTheme.monoFamily
                                font.pixelSize: 21
                                font.weight: Font.DemiBold
                            }
                        }
                    }
                }
            }

            IslandSectionLabel {
                Layout.fillWidth: true
                visible: statisticsPage.effectiveAdvancedVisible
                    && !statisticsPage.receiptSnapshotMode
                    && statisticsPage.selectedSection === 0
                text: qsTr("Top models")
            }

            Flow {
                Layout.fillWidth: true
                Layout.preferredHeight: childrenRect.height
                spacing: Kirigami.Units.smallSpacing
                visible: statisticsPage.effectiveAdvancedVisible
                    && !statisticsPage.receiptSnapshotMode
                    && statisticsPage.selectedSection === 0

                Repeater {
                    model: statisticsPage.models.slice(0, 3)

                    delegate: Rectangle {
                        id: topModelChip
                        required property var modelData
                        radius: 0
                        color: IslandTheme.surfaceRaised
                        border.color: IslandTheme.border
                        border.width: 1
                        implicitWidth: topModelLabel.implicitWidth + Kirigami.Units.largeSpacing
                        implicitHeight: topModelLabel.implicitHeight + Kirigami.Units.smallSpacing * 2

                        Label {
                            id: topModelLabel
                            anchors.centerIn: parent
                            text: statisticsPage.modelTitle(topModelChip.modelData) + " · "
                                    + statisticsPage.optionalNumber(topModelChip.modelData.requests)
                            maximumLineCount: 1
                            elide: Text.ElideRight
                            color: IslandTheme.primaryText
                            font.family: IslandTheme.monoFamily
                            font.pixelSize: 10
                            font.weight: Font.DemiBold
                        }
                    }
                }

                Label {
                    visible: statisticsPage.models.length === 0
                    text: statisticsPage.unavailableText
                    color: IslandTheme.secondaryText
                }
            }
        }

        ColumnLayout {
            objectName: "statistics-account-overview"
            Layout.fillWidth: true
            spacing: Kirigami.Units.smallSpacing

            IslandSectionLabel {
                text: qsTr("Account overview")
            }

            Repeater {
                model: statisticsPage.health

                delegate: IslandCard {
                    id: accountOverviewCard
                    required property var modelData
                    readonly property var account: statisticsPage.objectOrEmpty(modelData)
                    readonly property string attentionReason: statisticsPage.healthSummaryReason(account)
                    Layout.fillWidth: true

                    contentItem: ColumnLayout {
                        spacing: 6

                        RowLayout {
                            Layout.fillWidth: true
                            Rectangle {
                                implicitWidth: 7
                                implicitHeight: 7
                                radius: width / 2
                                color: accountOverviewCard.account.healthy === false
                                    ? IslandTheme.red
                                    : accountOverviewCard.account.paused === true
                                        ? IslandTheme.amber : IslandTheme.secondaryText
                            }
                            Label {
                                Layout.fillWidth: true
                                text: statisticsPage.optionalText(
                                    statisticsPage.firstAvailable(accountOverviewCard.account,
                                                                  ["display_name", "name"])
                                )
                                font.weight: Font.DemiBold
                                elide: Text.ElideMiddle
                            }
                            Label {
                                text: statisticsPage.optionalText(accountOverviewCard.account.status)
                                color: accountOverviewCard.account.healthy === false
                                    ? IslandTheme.red : IslandTheme.secondaryText
                            }
                        }

                        Label {
                            Layout.fillWidth: true
                            visible: accountOverviewCard.attentionReason.length > 0
                            text: accountOverviewCard.attentionReason
                            textFormat: Text.PlainText
                            color: accountOverviewCard.account.healthy === false
                                ? IslandTheme.red : IslandTheme.amber
                            wrapMode: Text.Wrap
                        }
                    }
                }
            }

            Label {
                visible: statisticsPage.health.length === 0
                Layout.fillWidth: true
                text: qsTr("Account overview is %1")
                    .arg(statisticsPage.unavailableText.toLowerCase())
                color: IslandTheme.secondaryText
                horizontalAlignment: Text.AlignHCenter
            }
        }

        IslandSeparator {
            Layout.fillWidth: true
            visible: statisticsPage.effectiveAdvancedVisible
                && !statisticsPage.receiptSnapshotMode
                && statisticsPage.selectedSection === 0
        }

        ColumnLayout {
            objectName: "statistics-heatmaps"
            Layout.fillWidth: true
            visible: statisticsPage.effectiveAdvancedVisible
                && !statisticsPage.receiptSnapshotMode
                && statisticsPage.selectedSection === 0
            spacing: Kirigami.Units.smallSpacing

            RowLayout {
                Layout.fillWidth: true
                IslandSectionLabel {
                    text: qsTr("Token heatmap")
                }
                Item { Layout.fillWidth: true }
                Label {
                    text: qsTr("Quality: %1").arg(statisticsPage.qualityValue("windowed"))
                    color: IslandTheme.secondaryText
                }
            }

            GridLayout {
                Layout.fillWidth: true
                columns: statisticsPage.width >= 760 ? 2 : 1
                columnSpacing: Kirigami.Units.smallSpacing
                rowSpacing: Kirigami.Units.smallSpacing

                Repeater {
                    model: ["24h", "72h"]

                    delegate: IslandCard {
                        id: heatmapCard
                        required property string modelData
                        readonly property var heatmap: statisticsPage.heatmapForWindow(modelData)
                        readonly property var cells: statisticsPage.arrayOrEmpty(heatmap.cells)
                        Layout.fillWidth: true

                        contentItem: ColumnLayout {
                            spacing: Kirigami.Units.smallSpacing
                            RowLayout {
                                Layout.fillWidth: true
                                IslandSectionLabel {
                                    text: heatmapCard.modelData
                                }
                                Item { Layout.fillWidth: true }
                                Label {
                                    text: qsTr("%1 cells").arg(heatmapCard.cells.length)
                                    color: IslandTheme.secondaryText
                                }
                            }

                            Repeater {
                                model: heatmapCard.cells

                                delegate: Rectangle {
                                    id: heatCell
                                    required property var modelData
                                    Layout.fillWidth: true
                                    radius: 0
                                    color: Qt.rgba(IslandTheme.primaryText.r,
                                                   IslandTheme.primaryText.g,
                                                   IslandTheme.primaryText.b,
                                                   statisticsPage.heatCellIntensity(modelData))
                                    implicitHeight: heatCellLayout.implicitHeight + Kirigami.Units.smallSpacing * 2

                                    Component.onCompleted: statisticsPage.renderedHeatmapCells += 1
                                    Component.onDestruction: statisticsPage.renderedHeatmapCells = Math.max(
                                        0, statisticsPage.renderedHeatmapCells - 1
                                    )

                                    RowLayout {
                                        id: heatCellLayout
                                        anchors.fill: parent
                                        anchors.margins: Kirigami.Units.smallSpacing
                                        ColumnLayout {
                                            Layout.fillWidth: true
                                            spacing: 0
                                            Label {
                                                Layout.fillWidth: true
                                                text: statisticsPage.modelTitle(heatCell.modelData)
                                                font.bold: true
                                                elide: Text.ElideRight
                                            }
                                            Label {
                                                Layout.fillWidth: true
                                                text: statisticsPage.optionalText(heatCell.modelData.account_display)
                                                color: IslandTheme.secondaryText
                                                elide: Text.ElideRight
                                            }
                                        }
                                        Label {
                                            text: qsTr("%1 tokens")
                                                    .arg(statisticsPage.totalTokensText(heatCell.modelData))
                                        }
                                        Label {
                                            text: qsTr("%1 req · %2 err")
                                                    .arg(statisticsPage.optionalNumber(heatCell.modelData.requests))
                                                    .arg(statisticsPage.optionalNumber(heatCell.modelData.errors))
                                            color: IslandTheme.secondaryText
                                        }
                                    }
                                }
                            }

                            Label {
                                visible: heatmapCard.cells.length === 0
                                Layout.fillWidth: true
                                text: qsTr("No %1 telemetry · %2")
                                        .arg(heatmapCard.modelData)
                                        .arg(statisticsPage.unavailableText)
                                color: IslandTheme.secondaryText
                                horizontalAlignment: Text.AlignHCenter
                            }
                        }
                    }
                }
            }
        }

        IslandSeparator {
            Layout.fillWidth: true
            visible: statisticsPage.effectiveAdvancedVisible
                && !statisticsPage.receiptSnapshotMode
                && statisticsPage.selectedSection === 0
        }

        ColumnLayout {
            objectName: "statistics-models"
            Layout.fillWidth: true
            visible: statisticsPage.effectiveAdvancedVisible
                && !statisticsPage.receiptSnapshotMode
                && statisticsPage.selectedSection === 0
            spacing: Kirigami.Units.smallSpacing

            RowLayout {
                Layout.fillWidth: true
                IslandSectionLabel {
                    text: qsTr("Models")
                }
                Item { Layout.fillWidth: true }
                Label {
                    text: qsTr("Cache: %1").arg(statisticsPage.qualityValue("cache"))
                    color: IslandTheme.secondaryText
                }
            }

            Repeater {
                model: statisticsPage.models

                delegate: IslandCard {
                    id: modelCard
                    required property var modelData
                    readonly property var accounts: statisticsPage.arrayOrEmpty(modelData.accounts)
                    Layout.fillWidth: true

                    contentItem: ColumnLayout {
                        spacing: Kirigami.Units.smallSpacing
                        RowLayout {
                            Layout.fillWidth: true
                            ColumnLayout {
                                Layout.fillWidth: true
                                spacing: 0
                                Label {
                                    Layout.fillWidth: true
                                    text: statisticsPage.modelTitle(modelCard.modelData)
                                    color: IslandTheme.primaryText
                                    font.family: IslandTheme.monoFamily
                                    font.pixelSize: 13
                                    font.weight: Font.DemiBold
                                    elide: Text.ElideRight
                                }
                                Label {
                                    text: qsTr("Last use: %1")
                                            .arg(statisticsPage.optionalTime(modelCard.modelData.last_used_ms))
                                    color: IslandTheme.secondaryText
                                }
                            }
                            Label {
                                text: qsTr("%1 in flight")
                                        .arg(statisticsPage.optionalNumber(modelCard.modelData.in_flight))
                                color: statisticsPage.hasValue(modelCard.modelData.in_flight)
                                        && Number(modelCard.modelData.in_flight) > 0
                                        ? IslandTheme.primaryText : IslandTheme.secondaryText
                            }
                        }

                        GridLayout {
                            Layout.fillWidth: true
                            columns: statisticsPage.width >= 760 ? 4 : 2
                            columnSpacing: Kirigami.Units.largeSpacing
                            Label { text: qsTr("Requests %1").arg(statisticsPage.optionalNumber(modelCard.modelData.requests)) }
                            Label { text: qsTr("OK %1").arg(statisticsPage.optionalNumber(modelCard.modelData.ok)); color: IslandTheme.primaryText }
                            Label { text: qsTr("Errors %1").arg(statisticsPage.optionalNumber(modelCard.modelData.errors)); color: IslandTheme.red }
                            Label { text: qsTr("Cost %1").arg(statisticsPage.optionalCurrency(modelCard.modelData.cost_usd)) }
                            Label { text: qsTr("Input %1").arg(statisticsPage.optionalNumber(modelCard.modelData.tokens_in)) }
                            Label { text: qsTr("Output %1").arg(statisticsPage.optionalNumber(modelCard.modelData.tokens_out)) }
                            Label { text: qsTr("Cache read %1").arg(statisticsPage.optionalNumber(modelCard.modelData.cache_read)) }
                            Label { text: qsTr("Cache create %1").arg(statisticsPage.optionalNumber(modelCard.modelData.cache_creation)) }
                        }

                        Label {
                            text: qsTr("Serving accounts")
                            font.bold: true
                        }

                        Repeater {
                            model: modelCard.accounts

                            delegate: RowLayout {
                                id: servingAccount
                                required property var modelData
                                Layout.fillWidth: true

                                Component.onCompleted: statisticsPage.renderedServingAccounts += 1
                                Component.onDestruction: statisticsPage.renderedServingAccounts = Math.max(
                                    0, statisticsPage.renderedServingAccounts - 1
                                )
                                Label {
                                    Layout.fillWidth: true
                                    text: statisticsPage.optionalText(servingAccount.modelData.display_name)
                                    elide: Text.ElideRight
                                }
                                Label {
                                    text: qsTr("%1 req · %2 ok · %3 err")
                                            .arg(statisticsPage.optionalNumber(servingAccount.modelData.requests))
                                            .arg(statisticsPage.optionalNumber(servingAccount.modelData.ok))
                                            .arg(statisticsPage.optionalNumber(servingAccount.modelData.errors))
                                    color: IslandTheme.secondaryText
                                }
                            }
                        }

                        Label {
                            visible: modelCard.accounts.length === 0
                            text: qsTr("Serving account data: %1").arg(statisticsPage.unavailableText)
                            color: IslandTheme.secondaryText
                        }
                    }
                }
            }

            Label {
                visible: statisticsPage.models.length === 0
                Layout.fillWidth: true
                text: qsTr("Model telemetry is %1").arg(statisticsPage.unavailableText.toLowerCase())
                color: IslandTheme.secondaryText
                horizontalAlignment: Text.AlignHCenter
            }
        }

        IslandSeparator {
            Layout.fillWidth: true
            visible: statisticsPage.effectiveAdvancedVisible
                && !statisticsPage.receiptSnapshotMode
                && statisticsPage.selectedSection === 1
        }

        ColumnLayout {
            objectName: "statistics-clients"
            Layout.fillWidth: true
            visible: statisticsPage.effectiveAdvancedVisible
                && !statisticsPage.receiptSnapshotMode
                && statisticsPage.selectedSection === 1
            spacing: Kirigami.Units.smallSpacing

            IslandSectionLabel {
                text: qsTr("Clients")
            }

            Repeater {
                model: statisticsPage.clients

                delegate: IslandCard {
                    id: clientCard
                    required property var modelData
                    Layout.fillWidth: true

                    contentItem: RowLayout {
                        spacing: Kirigami.Units.largeSpacing
                        ColumnLayout {
                            Layout.fillWidth: true
                            spacing: 0
                            Label {
                                Layout.fillWidth: true
                                text: statisticsPage.abbreviatedClient(clientCard.modelData.client)
                                font.bold: true
                                font.family: "monospace"
                                elide: Text.ElideMiddle
                            }
                            Label {
                                text: qsTr("Last seen: %1")
                                        .arg(statisticsPage.optionalTime(clientCard.modelData.last_seen_ms))
                                color: IslandTheme.secondaryText
                            }
                        }
                        Label { text: qsTr("%1 requests").arg(statisticsPage.optionalNumber(clientCard.modelData.requests)) }
                        Label { text: qsTr("%1 tokens").arg(statisticsPage.totalTokensText(clientCard.modelData)) }
                        Label {
                            text: qsTr("%1 errors").arg(statisticsPage.optionalNumber(clientCard.modelData.errors))
                            color: statisticsPage.hasValue(clientCard.modelData.errors) && Number(clientCard.modelData.errors) > 0
                                    ? IslandTheme.red : IslandTheme.secondaryText
                        }
                        Label { text: statisticsPage.optionalCurrency(clientCard.modelData.cost_usd) }
                    }
                }
            }

            Label {
                visible: statisticsPage.clients.length === 0
                Layout.fillWidth: true
                text: qsTr("Client telemetry is %1").arg(statisticsPage.unavailableText.toLowerCase())
                color: IslandTheme.secondaryText
                horizontalAlignment: Text.AlignHCenter
            }
        }

        IslandSeparator {
            Layout.fillWidth: true
            visible: statisticsPage.effectiveAdvancedVisible
                && !statisticsPage.receiptSnapshotMode
                && statisticsPage.selectedSection === 2
        }

        ColumnLayout {
            objectName: "statistics-health"
            Layout.fillWidth: true
            visible: statisticsPage.effectiveAdvancedVisible
                && !statisticsPage.receiptSnapshotMode
                && statisticsPage.selectedSection === 2
            spacing: Kirigami.Units.smallSpacing

            IslandSectionLabel {
                text: qsTr("Health")
            }

            Repeater {
                model: statisticsPage.health

                delegate: IslandCard {
                    id: healthCard
                    required property var modelData
                    Layout.fillWidth: true

                    contentItem: ColumnLayout {
                        spacing: Kirigami.Units.smallSpacing
                        RowLayout {
                            Layout.fillWidth: true
                            Rectangle {
                                implicitWidth: Kirigami.Units.smallSpacing
                                implicitHeight: healthName.implicitHeight
                                radius: width / 2
                                color: healthCard.modelData.paused === true
                                    ? IslandTheme.amber
                                    : healthCard.modelData.healthy
                                        ? IslandTheme.secondaryText : IslandTheme.red
                            }
                            Label {
                                id: healthName
                                Layout.fillWidth: true
                                text: statisticsPage.optionalText(
                                    statisticsPage.firstAvailable(healthCard.modelData,
                                                                  ["display_name", "name"])
                                )
                                font.bold: true
                                elide: Text.ElideRight
                            }
                            Label {
                                text: statisticsPage.optionalText(healthCard.modelData.status)
                            }
                            Label {
                                visible: healthCard.modelData.paused === true
                                text: qsTr("Paused")
                                color: IslandTheme.amber
                            }
                        }

                        GridLayout {
                            Layout.fillWidth: true
                            columns: statisticsPage.width >= 760 ? 3 : 1
                            columnSpacing: Kirigami.Units.largeSpacing
                            Label { text: qsTr("Credential: %1").arg(statisticsPage.healthCredential(healthCard.modelData)) }
                            Label { text: qsTr("Cooldown / block: %1").arg(statisticsPage.healthCooldown(healthCard.modelData)) }
                            Label { text: qsTr("In flight: %1").arg(statisticsPage.optionalNumber(healthCard.modelData.in_flight)) }
                            Label { text: qsTr("Token expiry: %1").arg(statisticsPage.optionalTime(healthCard.modelData.token_expires_at_ms)) }
                            Label { text: qsTr("Refresh: %1").arg(statisticsPage.healthRefresh(healthCard.modelData)) }
                        }
                    }
                }
            }

            Label {
                visible: statisticsPage.health.length === 0
                Layout.fillWidth: true
                text: qsTr("Health telemetry is %1").arg(statisticsPage.unavailableText.toLowerCase())
                color: IslandTheme.secondaryText
                horizontalAlignment: Text.AlignHCenter
            }
        }

        IslandSeparator {
            Layout.fillWidth: true
            visible: statisticsPage.receiptSnapshotMode
                || (statisticsPage.effectiveAdvancedVisible
                    && statisticsPage.selectedSection === 3)
        }

        ColumnLayout {
            id: receiptEvidenceSection
            objectName: "statistics-activity-receipts"
            Layout.fillWidth: true
            visible: statisticsPage.receiptSnapshotMode
                || (statisticsPage.effectiveAdvancedVisible
                    && statisticsPage.selectedSection === 3)
            spacing: Kirigami.Units.smallSpacing

            RowLayout {
                Layout.fillWidth: true
                IslandSectionLabel {
                    text: qsTr("Request receipts")
                }
                Item { Layout.fillWidth: true }
                Label {
                    text: qsTr("%1 visible").arg(statisticsPage.activityReceipts.length)
                    color: IslandTheme.secondaryText
                    font.family: IslandTheme.monoFamily
                    font.pixelSize: 10
                }
            }

            Label {
                Layout.fillWidth: true
                text: qsTr("Metadata only. Request and response content is never shown.")
                color: IslandTheme.secondaryText
                wrapMode: Text.Wrap
            }

            Repeater {
                model: statisticsPage.activityReceipts

                delegate: IslandCard {
                    id: receiptCard
                    required property var modelData
                    readonly property var receipt: statisticsPage.objectOrEmpty(modelData)
                    Layout.fillWidth: true

                    contentItem: ColumnLayout {
                        spacing: 6
                        RowLayout {
                            Layout.fillWidth: true
                            Rectangle {
                                radius: 0
                                implicitWidth: receiptStatus.implicitWidth + Kirigami.Units.largeSpacing
                                implicitHeight: receiptStatus.implicitHeight + Kirigami.Units.smallSpacing
                                color: statisticsPage.receiptStatusColor(receiptCard.receipt)

                                Label {
                                    id: receiptStatus
                                    anchors.centerIn: parent
                                    text: statisticsPage.receiptStatusText(receiptCard.receipt)
                                    color: receiptCard.receipt.error
                                        ? IslandTheme.red : IslandTheme.primaryText
                                    font.family: IslandTheme.monoFamily
                                    font.pixelSize: 9
                                    font.weight: Font.DemiBold
                                }
                            }
                            ColumnLayout {
                                Layout.fillWidth: true
                                spacing: 0
                                Label {
                                    Layout.fillWidth: true
                                    text: statisticsPage.receiptHeadline(receiptCard.receipt)
                                    font.bold: true
                                    font.family: IslandTheme.monoFamily
                                    font.pixelSize: 11
                                    elide: Text.ElideRight
                                }
                                Label {
                                    Layout.fillWidth: true
                                    text: statisticsPage.optionalText(receiptCard.receipt.receipt_id)
                                            + " · " + statisticsPage.optionalTime(receiptCard.receipt.occurred_at_ms)
                                    color: IslandTheme.secondaryText
                                    font.family: IslandTheme.monoFamily
                                    font.pixelSize: 9
                                    elide: Text.ElideMiddle
                                }
                            }
                            Label {
                                text: statisticsPage.receiptTiming(receiptCard.receipt)
                                color: IslandTheme.secondaryText
                                font.family: IslandTheme.monoFamily
                                font.pixelSize: 10
                            }
                        }

                        Label {
                            visible: receiptCard.receipt.kind !== "note"
                            Layout.fillWidth: true
                            text: statisticsPage.receiptTarget(receiptCard.receipt)
                            color: IslandTheme.secondaryText
                            font.family: IslandTheme.monoFamily
                            font.pixelSize: 10
                            elide: Text.ElideRight
                        }

                        Flow {
                            visible: receiptCard.receipt.kind !== "note"
                            Layout.fillWidth: true
                            Layout.preferredHeight: visible ? childrenRect.height : 0
                            spacing: Kirigami.Units.smallSpacing

                            Label {
                                text: qsTr("Effort: %1").arg(statisticsPage.optionalText(receiptCard.receipt.effort))
                                font.family: IslandTheme.monoFamily
                                font.pixelSize: 9
                            }
                            Label {
                                text: receiptCard.receipt.fast ? qsTr("Fast") : qsTr("Standard")
                                color: receiptCard.receipt.fast
                                        ? IslandTheme.primaryText : IslandTheme.secondaryText
                                font.family: IslandTheme.monoFamily
                                font.pixelSize: 9
                            }
                            Label {
                                text: qsTr("Input: %1").arg(statisticsPage.receiptTokenInput(receiptCard.receipt))
                                font.family: IslandTheme.monoFamily
                                font.pixelSize: 9
                            }
                            Label {
                                text: qsTr("Output: %1").arg(statisticsPage.receiptTokenOutput(receiptCard.receipt))
                                font.family: IslandTheme.monoFamily
                                font.pixelSize: 9
                            }
                            Label {
                                text: qsTr("Cache read: %1").arg(statisticsPage.receiptCacheRead(receiptCard.receipt))
                                font.family: IslandTheme.monoFamily
                                font.pixelSize: 9
                            }
                            Label {
                                text: qsTr("Cache create: %1").arg(statisticsPage.receiptCacheCreation(receiptCard.receipt))
                                font.family: IslandTheme.monoFamily
                                font.pixelSize: 9
                            }
                            Label {
                                text: qsTr("Cost: %1").arg(statisticsPage.optionalCurrency(receiptCard.receipt.cost_usd))
                                font.family: IslandTheme.monoFamily
                                font.pixelSize: 9
                            }
                        }

                        Label {
                            visible: receiptCard.receipt.kind === "note"
                            Layout.fillWidth: true
                            text: statisticsPage.optionalText(receiptCard.receipt.message)
                            color: receiptCard.receipt.error
                                    ? IslandTheme.red : IslandTheme.primaryText
                            wrapMode: Text.Wrap
                            font.pixelSize: 10
                        }
                    }
                }
            }

            Label {
                visible: statisticsPage.activityReceipts.length === 0
                Layout.fillWidth: true
                text: qsTr("Request receipt telemetry is %1")
                        .arg(statisticsPage.unavailableText.toLowerCase())
                color: IslandTheme.secondaryText
                horizontalAlignment: Text.AlignHCenter
            }

            IslandSeparator {
                visible: statisticsPage.verificationReceipts.length > 0
                Layout.fillWidth: true
            }

            IslandSectionLabel {
                visible: statisticsPage.verificationReceipts.length > 0
                text: qsTr("Verification receipts")
            }

            Repeater {
                model: statisticsPage.verificationReceipts

                delegate: IslandCard {
                    id: verificationCard
                    required property var modelData
                    readonly property var receipt: statisticsPage.objectOrEmpty(modelData)
                    Layout.fillWidth: true

                    contentItem: RowLayout {
                        spacing: Kirigami.Units.largeSpacing

                        Rectangle {
                            radius: width / 2
                            implicitWidth: Kirigami.Units.smallSpacing
                            implicitHeight: implicitWidth
                            color: statisticsPage.verificationOutcomeColor(
                                verificationCard.receipt.outcome
                            )
                        }

                        ColumnLayout {
                            Layout.fillWidth: true
                            spacing: 0
                            Label {
                                Layout.fillWidth: true
                                text: statisticsPage.optionalText(
                                    verificationCard.receipt.operation
                                ) + " · " + statisticsPage.optionalText(
                                    verificationCard.receipt.outcome
                                )
                                font.bold: true
                                font.pixelSize: 10
                                elide: Text.ElideRight
                            }
                            Label {
                                Layout.fillWidth: true
                                text: statisticsPage.optionalText(
                                    verificationCard.receipt.target_display
                                ) + " · " + statisticsPage.optionalText(
                                    verificationCard.receipt.message
                                )
                                color: IslandTheme.secondaryText
                                wrapMode: Text.Wrap
                                font.pixelSize: 10
                            }
                        }

                        Label {
                            text: statisticsPage.optionalTime(
                                verificationCard.receipt.finished_at_ms
                            )
                            color: IslandTheme.secondaryText
                            font.family: IslandTheme.monoFamily
                            font.pixelSize: 9
                        }
                    }
                }
            }
        }
    }
}
