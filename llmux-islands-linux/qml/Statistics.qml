pragma ComponentBehavior: Bound

import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

Kirigami.ScrollablePage {
    id: statisticsPage

    property var uiState: ({})
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
    readonly property string unavailableText: qsTr("Unavailable")
    readonly property real preferredContentHeight: statisticsContent.implicitHeight

    title: qsTr("Statistics")

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
        case "succeeded": return Kirigami.Theme.positiveTextColor
        case "failed": return Kirigami.Theme.negativeTextColor
        case "cancelled": return Kirigami.Theme.neutralTextColor
        default: return Kirigami.Theme.disabledTextColor
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

    function receiptStatusText(receipt) {
        if (receipt.kind === "in_flight")
            return qsTr("In flight")
        if (receipt.kind === "note")
            return receipt.error ? qsTr("Error note") : qsTr("Note")
        return hasValue(receipt.status) ? String(receipt.status) : qsTr("Completed")
    }

    function receiptStatusColor(receipt) {
        if (receipt.kind === "in_flight")
            return Kirigami.Theme.neutralBackgroundColor
        if (receipt.error || (hasValue(receipt.status) && Number(receipt.status) >= 400))
            return Kirigami.Theme.negativeBackgroundColor
        if (receipt.kind === "note")
            return Kirigami.Theme.alternateBackgroundColor
        return Kirigami.Theme.positiveBackgroundColor
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
        spacing: Kirigami.Units.largeSpacing

        Kirigami.Heading {
            text: qsTr("Statistics")
            level: 1
        }

        ColumnLayout {
            objectName: "statistics-overview"
            Layout.fillWidth: true
            spacing: Kirigami.Units.smallSpacing

            RowLayout {
                Layout.fillWidth: true
                Kirigami.Heading {
                    text: qsTr("Overview")
                    level: 2
                }
                Item { Layout.fillWidth: true }
                Label {
                    text: qsTr("Model data: %1 · Cost: %2")
                            .arg(statisticsPage.qualityValue("model_usage"))
                            .arg(statisticsPage.qualityValue("cost"))
                    color: Kirigami.Theme.disabledTextColor
                    elide: Text.ElideRight
                }
            }

            GridLayout {
                Layout.fillWidth: true
                columns: statisticsPage.width >= 760 ? 5 : 2
                columnSpacing: Kirigami.Units.smallSpacing
                rowSpacing: Kirigami.Units.smallSpacing

                Repeater {
                    model: [
                        { "label": qsTr("Requests"), "value": statisticsPage.optionalNumber(statisticsPage.overview.requests) },
                        { "label": qsTr("Tokens"), "value": statisticsPage.overviewTokensText() },
                        { "label": qsTr("API-equivalent cost"), "value": statisticsPage.optionalCurrency(statisticsPage.overview.cost_usd) },
                        { "label": qsTr("Error rate"), "value": statisticsPage.errorRateText() },
                        { "label": qsTr("Accounts"), "value": statisticsPage.accountCountText() }
                    ]

                    delegate: Kirigami.AbstractCard {
                        id: overviewCard
                        required property var modelData
                        Layout.fillWidth: true

                        contentItem: ColumnLayout {
                            spacing: 0
                            Label {
                                text: overviewCard.modelData.label
                                color: Kirigami.Theme.disabledTextColor
                            }
                            Kirigami.Heading {
                                text: overviewCard.modelData.value
                                level: 2
                            }
                        }
                    }
                }
            }

            Label {
                Layout.fillWidth: true
                text: qsTr("Top models")
                font.bold: true
            }

            Flow {
                Layout.fillWidth: true
                Layout.preferredHeight: childrenRect.height
                spacing: Kirigami.Units.smallSpacing

                Repeater {
                    model: statisticsPage.models.slice(0, 3)

                    delegate: Rectangle {
                        id: topModelChip
                        required property var modelData
                        radius: Kirigami.Units.smallSpacing
                        color: Kirigami.Theme.alternateBackgroundColor
                        implicitWidth: topModelLabel.implicitWidth + Kirigami.Units.largeSpacing
                        implicitHeight: topModelLabel.implicitHeight + Kirigami.Units.smallSpacing * 2

                        Label {
                            id: topModelLabel
                            anchors.centerIn: parent
                            text: statisticsPage.modelTitle(topModelChip.modelData) + " · "
                                    + statisticsPage.optionalNumber(topModelChip.modelData.requests)
                            maximumLineCount: 1
                            elide: Text.ElideRight
                        }
                    }
                }

                Label {
                    visible: statisticsPage.models.length === 0
                    text: statisticsPage.unavailableText
                    color: Kirigami.Theme.disabledTextColor
                }
            }
        }

        Kirigami.Separator { Layout.fillWidth: true }

        ColumnLayout {
            objectName: "statistics-heatmaps"
            Layout.fillWidth: true
            spacing: Kirigami.Units.smallSpacing

            RowLayout {
                Layout.fillWidth: true
                Kirigami.Heading {
                    text: qsTr("Token heatmap")
                    level: 2
                }
                Item { Layout.fillWidth: true }
                Label {
                    text: qsTr("Quality: %1").arg(statisticsPage.qualityValue("windowed"))
                    color: Kirigami.Theme.disabledTextColor
                }
            }

            GridLayout {
                Layout.fillWidth: true
                columns: statisticsPage.width >= 760 ? 2 : 1
                columnSpacing: Kirigami.Units.smallSpacing
                rowSpacing: Kirigami.Units.smallSpacing

                Repeater {
                    model: ["24h", "72h"]

                    delegate: Kirigami.AbstractCard {
                        id: heatmapCard
                        required property string modelData
                        readonly property var heatmap: statisticsPage.heatmapForWindow(modelData)
                        readonly property var cells: statisticsPage.arrayOrEmpty(heatmap.cells)
                        Layout.fillWidth: true

                        contentItem: ColumnLayout {
                            spacing: Kirigami.Units.smallSpacing
                            RowLayout {
                                Layout.fillWidth: true
                                Kirigami.Heading {
                                    text: heatmapCard.modelData
                                    level: 3
                                }
                                Item { Layout.fillWidth: true }
                                Label {
                                    text: qsTr("%1 cells").arg(heatmapCard.cells.length)
                                    color: Kirigami.Theme.disabledTextColor
                                }
                            }

                            Repeater {
                                model: heatmapCard.cells

                                delegate: Rectangle {
                                    id: heatCell
                                    required property var modelData
                                    Layout.fillWidth: true
                                    radius: Kirigami.Units.smallSpacing
                                    color: Qt.rgba(Kirigami.Theme.highlightColor.r,
                                                   Kirigami.Theme.highlightColor.g,
                                                   Kirigami.Theme.highlightColor.b,
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
                                                color: Kirigami.Theme.disabledTextColor
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
                                            color: Kirigami.Theme.disabledTextColor
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
                                color: Kirigami.Theme.disabledTextColor
                                horizontalAlignment: Text.AlignHCenter
                            }
                        }
                    }
                }
            }
        }

        Kirigami.Separator { Layout.fillWidth: true }

        ColumnLayout {
            objectName: "statistics-models"
            Layout.fillWidth: true
            spacing: Kirigami.Units.smallSpacing

            RowLayout {
                Layout.fillWidth: true
                Kirigami.Heading {
                    text: qsTr("Models")
                    level: 2
                }
                Item { Layout.fillWidth: true }
                Label {
                    text: qsTr("Cache: %1").arg(statisticsPage.qualityValue("cache"))
                    color: Kirigami.Theme.disabledTextColor
                }
            }

            Repeater {
                model: statisticsPage.models

                delegate: Kirigami.AbstractCard {
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
                                Kirigami.Heading {
                                    Layout.fillWidth: true
                                    text: statisticsPage.modelTitle(modelCard.modelData)
                                    level: 3
                                    elide: Text.ElideRight
                                }
                                Label {
                                    text: qsTr("Last use: %1")
                                            .arg(statisticsPage.optionalTime(modelCard.modelData.last_used_ms))
                                    color: Kirigami.Theme.disabledTextColor
                                }
                            }
                            Label {
                                text: qsTr("%1 in flight")
                                        .arg(statisticsPage.optionalNumber(modelCard.modelData.in_flight))
                                color: statisticsPage.hasValue(modelCard.modelData.in_flight)
                                        && Number(modelCard.modelData.in_flight) > 0
                                        ? Kirigami.Theme.highlightColor : Kirigami.Theme.disabledTextColor
                            }
                        }

                        GridLayout {
                            Layout.fillWidth: true
                            columns: statisticsPage.width >= 760 ? 4 : 2
                            columnSpacing: Kirigami.Units.largeSpacing
                            Label { text: qsTr("Requests %1").arg(statisticsPage.optionalNumber(modelCard.modelData.requests)) }
                            Label { text: qsTr("OK %1").arg(statisticsPage.optionalNumber(modelCard.modelData.ok)); color: Kirigami.Theme.positiveTextColor }
                            Label { text: qsTr("Errors %1").arg(statisticsPage.optionalNumber(modelCard.modelData.errors)); color: Kirigami.Theme.negativeTextColor }
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
                                    color: Kirigami.Theme.disabledTextColor
                                }
                            }
                        }

                        Label {
                            visible: modelCard.accounts.length === 0
                            text: qsTr("Serving account data: %1").arg(statisticsPage.unavailableText)
                            color: Kirigami.Theme.disabledTextColor
                        }
                    }
                }
            }

            Label {
                visible: statisticsPage.models.length === 0
                Layout.fillWidth: true
                text: qsTr("Model telemetry is %1").arg(statisticsPage.unavailableText.toLowerCase())
                color: Kirigami.Theme.disabledTextColor
                horizontalAlignment: Text.AlignHCenter
            }
        }

        Kirigami.Separator { Layout.fillWidth: true }

        ColumnLayout {
            objectName: "statistics-clients"
            Layout.fillWidth: true
            spacing: Kirigami.Units.smallSpacing

            Kirigami.Heading {
                text: qsTr("Clients")
                level: 2
            }

            Repeater {
                model: statisticsPage.clients

                delegate: Kirigami.AbstractCard {
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
                                color: Kirigami.Theme.disabledTextColor
                            }
                        }
                        Label { text: qsTr("%1 requests").arg(statisticsPage.optionalNumber(clientCard.modelData.requests)) }
                        Label { text: qsTr("%1 tokens").arg(statisticsPage.totalTokensText(clientCard.modelData)) }
                        Label {
                            text: qsTr("%1 errors").arg(statisticsPage.optionalNumber(clientCard.modelData.errors))
                            color: statisticsPage.hasValue(clientCard.modelData.errors) && Number(clientCard.modelData.errors) > 0
                                    ? Kirigami.Theme.negativeTextColor : Kirigami.Theme.disabledTextColor
                        }
                        Label { text: statisticsPage.optionalCurrency(clientCard.modelData.cost_usd) }
                    }
                }
            }

            Label {
                visible: statisticsPage.clients.length === 0
                Layout.fillWidth: true
                text: qsTr("Client telemetry is %1").arg(statisticsPage.unavailableText.toLowerCase())
                color: Kirigami.Theme.disabledTextColor
                horizontalAlignment: Text.AlignHCenter
            }
        }

        Kirigami.Separator { Layout.fillWidth: true }

        ColumnLayout {
            objectName: "statistics-health"
            Layout.fillWidth: true
            spacing: Kirigami.Units.smallSpacing

            Kirigami.Heading {
                text: qsTr("Health")
                level: 2
            }

            Repeater {
                model: statisticsPage.health

                delegate: Kirigami.AbstractCard {
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
                                color: healthCard.modelData.healthy && !healthCard.modelData.paused
                                        ? Kirigami.Theme.positiveTextColor
                                        : Kirigami.Theme.negativeTextColor
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
                                color: Kirigami.Theme.neutralTextColor
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
                color: Kirigami.Theme.disabledTextColor
                horizontalAlignment: Text.AlignHCenter
            }
        }

        Kirigami.Separator { Layout.fillWidth: true }

        ColumnLayout {
            id: receiptEvidenceSection
            objectName: "statistics-activity-receipts"
            Layout.fillWidth: true
            spacing: Kirigami.Units.smallSpacing

            RowLayout {
                Layout.fillWidth: true
                Kirigami.Heading {
                    text: qsTr("Request receipts")
                    level: 2
                }
                Item { Layout.fillWidth: true }
                Label {
                    text: qsTr("%1 visible").arg(statisticsPage.activityReceipts.length)
                    color: Kirigami.Theme.disabledTextColor
                }
            }

            Label {
                Layout.fillWidth: true
                text: qsTr("Metadata only. Request and response content is never shown.")
                color: Kirigami.Theme.disabledTextColor
                wrapMode: Text.Wrap
            }

            Repeater {
                model: statisticsPage.activityReceipts

                delegate: Kirigami.AbstractCard {
                    id: receiptCard
                    required property var modelData
                    readonly property var receipt: statisticsPage.objectOrEmpty(modelData)
                    Layout.fillWidth: true

                    contentItem: ColumnLayout {
                        spacing: Kirigami.Units.smallSpacing
                        RowLayout {
                            Layout.fillWidth: true
                            Rectangle {
                                radius: height / 2
                                implicitWidth: receiptStatus.implicitWidth + Kirigami.Units.largeSpacing
                                implicitHeight: receiptStatus.implicitHeight + Kirigami.Units.smallSpacing
                                color: statisticsPage.receiptStatusColor(receiptCard.receipt)

                                Label {
                                    id: receiptStatus
                                    anchors.centerIn: parent
                                    text: statisticsPage.receiptStatusText(receiptCard.receipt)
                                }
                            }
                            ColumnLayout {
                                Layout.fillWidth: true
                                spacing: 0
                                Label {
                                    Layout.fillWidth: true
                                    text: statisticsPage.receiptHeadline(receiptCard.receipt)
                                    font.bold: true
                                    elide: Text.ElideRight
                                }
                                Label {
                                    Layout.fillWidth: true
                                    text: statisticsPage.optionalText(receiptCard.receipt.receipt_id)
                                            + " · " + statisticsPage.optionalTime(receiptCard.receipt.occurred_at_ms)
                                    color: Kirigami.Theme.disabledTextColor
                                    font.family: "monospace"
                                    elide: Text.ElideMiddle
                                }
                            }
                            Label {
                                text: statisticsPage.receiptTiming(receiptCard.receipt)
                                color: Kirigami.Theme.disabledTextColor
                            }
                        }

                        Label {
                            visible: receiptCard.receipt.kind !== "note"
                            Layout.fillWidth: true
                            text: statisticsPage.receiptTarget(receiptCard.receipt)
                            color: Kirigami.Theme.disabledTextColor
                            elide: Text.ElideRight
                        }

                        Flow {
                            visible: receiptCard.receipt.kind !== "note"
                            Layout.fillWidth: true
                            Layout.preferredHeight: visible ? childrenRect.height : 0
                            spacing: Kirigami.Units.smallSpacing

                            Label {
                                text: qsTr("Effort: %1").arg(statisticsPage.optionalText(receiptCard.receipt.effort))
                            }
                            Label {
                                text: receiptCard.receipt.fast ? qsTr("Fast") : qsTr("Standard")
                                color: receiptCard.receipt.fast
                                        ? Kirigami.Theme.highlightColor : Kirigami.Theme.disabledTextColor
                            }
                            Label {
                                text: qsTr("Input: %1").arg(statisticsPage.receiptTokenInput(receiptCard.receipt))
                            }
                            Label {
                                text: qsTr("Output: %1").arg(statisticsPage.receiptTokenOutput(receiptCard.receipt))
                            }
                            Label {
                                text: qsTr("Cache read: %1").arg(statisticsPage.receiptCacheRead(receiptCard.receipt))
                            }
                            Label {
                                text: qsTr("Cache create: %1").arg(statisticsPage.receiptCacheCreation(receiptCard.receipt))
                            }
                            Label {
                                text: qsTr("Cost: %1").arg(statisticsPage.optionalCurrency(receiptCard.receipt.cost_usd))
                            }
                        }

                        Label {
                            visible: receiptCard.receipt.kind === "note"
                            Layout.fillWidth: true
                            text: statisticsPage.optionalText(receiptCard.receipt.message)
                            color: receiptCard.receipt.error
                                    ? Kirigami.Theme.negativeTextColor : Kirigami.Theme.textColor
                            wrapMode: Text.Wrap
                        }
                    }
                }
            }

            Label {
                visible: statisticsPage.activityReceipts.length === 0
                Layout.fillWidth: true
                text: qsTr("Request receipt telemetry is %1")
                        .arg(statisticsPage.unavailableText.toLowerCase())
                color: Kirigami.Theme.disabledTextColor
                horizontalAlignment: Text.AlignHCenter
            }

            Kirigami.Separator {
                visible: statisticsPage.verificationReceipts.length > 0
                Layout.fillWidth: true
            }

            Kirigami.Heading {
                visible: statisticsPage.verificationReceipts.length > 0
                text: qsTr("Verification receipts")
                level: 2
            }

            Repeater {
                model: statisticsPage.verificationReceipts

                delegate: Kirigami.AbstractCard {
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
                                elide: Text.ElideRight
                            }
                            Label {
                                Layout.fillWidth: true
                                text: statisticsPage.optionalText(
                                    verificationCard.receipt.target_display
                                ) + " · " + statisticsPage.optionalText(
                                    verificationCard.receipt.message
                                )
                                color: Kirigami.Theme.disabledTextColor
                                wrapMode: Text.Wrap
                            }
                        }

                        Label {
                            text: statisticsPage.optionalTime(
                                verificationCard.receipt.finished_at_ms
                            )
                            color: Kirigami.Theme.disabledTextColor
                        }
                    }
                }
            }
        }
    }
}
