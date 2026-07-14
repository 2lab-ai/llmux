pragma ComponentBehavior: Bound

import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

Kirigami.ScrollablePage {
    id: usagePage

    property var uiState: ({})
    property var removeCandidate: ({})
    property int renderedGaugeRows: 0
    property alias snapshotReceiptTarget: verificationReceiptSection
    readonly property var usage: objectOrEmpty(uiState.usage)
    readonly property var settings: objectOrEmpty(uiState.settings)
    readonly property var connection: objectOrEmpty(uiState.connection)
    readonly property var accounts: arrayOrEmpty(usage.accounts)
    readonly property var currentByGroup: objectOrEmpty(usage.current_by_group)
    readonly property var providerInFlight: objectOrEmpty(usage.provider_in_flight)
    readonly property var login: objectOrEmpty(usage.login)
    readonly property var verificationReceipts: usageReceipts(uiState.verification_receipts)
    readonly property string unavailableText: qsTr("Unavailable")
    readonly property bool isOffline: uiState.lifecycle === "offline"
            || uiState.lifecycle === "fatal"
    readonly property bool isStarting: uiState.lifecycle === "starting"
    readonly property bool loginCancelling: login.phase === "cancelling"
    readonly property bool loginActive: login.phase === "starting"
            || login.phase === "pending" || loginCancelling
    readonly property real preferredContentHeight: usageContent.implicitHeight
    signal dispatchRequested(string action, string payloadJson)

    title: qsTr("Usage")

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

    function clampFraction(value) {
        if (!hasValue(value))
            return 0
        var number = Number(value)
        if (!isFinite(number))
            return 0
        return Math.max(0, Math.min(1, number))
    }

    function optionalText(value) {
        return hasValue(value) ? String(value) : unavailableText
    }

    function optionalTime(value) {
        if (!hasValue(value))
            return unavailableText
        var milliseconds = Number(value)
        if (!isFinite(milliseconds) || milliseconds < 0)
            return unavailableText
        var date = new Date(milliseconds)
        return isNaN(date.getTime())
                ? unavailableText : Qt.formatDateTime(date, "yyyy-MM-dd hh:mm:ss")
    }

    function providerLabel(value) {
        switch (String(value).toLowerCase()) {
        case "claude": return qsTr("Claude")
        case "codex": return qsTr("Codex")
        case "grok": return qsTr("Grok")
        case "api": return qsTr("API key")
        default: return qsTr("Unknown provider")
        }
    }

    function providerIcon(value) {
        switch (String(value).toLowerCase()) {
        case "claude": return "draw-spiral"
        case "codex": return "code-context"
        case "grok": return "network-connect"
        case "api": return "password-show-off"
        default: return "dialog-question"
        }
    }

    function accountDisplay(account) {
        account = objectOrEmpty(account)
        return optionalText(account.display_name)
    }

    function accountIsCurrent(account) {
        account = objectOrEmpty(account)
        if (hasOwn(account, "current"))
            return account.current === true
        if (!hasValue(account.id))
            return false
        var groups = Object.keys(currentByGroup)
        for (var index = 0; index < groups.length; index += 1) {
            if (currentByGroup[groups[index]] === account.id)
                return true
        }
        return false
    }

    function providerCounters() {
        var rows = []
        var keys = Object.keys(providerInFlight).sort()
        for (var index = 0; index < keys.length; index += 1) {
            var count = Number(providerInFlight[keys[index]])
            if (isFinite(count) && count > 0) {
                rows.push({
                    "provider": keys[index],
                    "count": Math.floor(count)
                })
            }
        }
        return rows
    }

    function visibleGauges(account) {
        account = objectOrEmpty(account)
        var gauges = arrayOrEmpty(account.gauges)
        return gauges.filter(function(gaugeValue) {
            var gauge = objectOrEmpty(gaugeValue)
            return gauge.kind !== "fable_weekly"
                    || settings.show_fable_weekly !== false
        })
    }

    function renderedGaugeCount() {
        return renderedGaugeRows
    }

    function renderedVerificationReceiptCount() {
        return verificationReceipts.length
    }

    function gaugeLabel(gauge) {
        gauge = objectOrEmpty(gauge)
        switch (gauge.kind) {
        case "five_hour": return qsTr("5 hour")
        case "seven_day": return qsTr("7 day")
        case "fable_weekly": return qsTr("Fable weekly")
        default: return qsTr("Quota")
        }
    }

    function gaugeRemaining(gauge) {
        gauge = objectOrEmpty(gauge)
        if (gauge.available !== true || !hasValue(gauge.remaining_fraction))
            return unavailableText
        return qsTr("%1% remaining").arg(Math.round(clampFraction(gauge.remaining_fraction) * 100))
    }

    function gaugeUsed(gauge) {
        gauge = objectOrEmpty(gauge)
        if (gauge.available !== true || !hasValue(gauge.used_fraction))
            return unavailableText
        return qsTr("%1% used").arg(Math.round(clampFraction(gauge.used_fraction) * 100))
    }

    function gaugeReset(gauge) {
        gauge = objectOrEmpty(gauge)
        if (hasValue(gauge.reset_text))
            return String(gauge.reset_text)
        if (hasValue(gauge.resets_at))
            return qsTr("Resets %1").arg(optionalTime(gauge.resets_at))
        return unavailableText
    }

    function gaugeColor(gauge, account) {
        gauge = objectOrEmpty(gauge)
        account = objectOrEmpty(account)
        if (gauge.constraining === true || account.warning_level === "critical")
            return Kirigami.Theme.negativeTextColor
        if (account.warning_level === "warning")
            return Kirigami.Theme.neutralTextColor
        return Kirigami.Theme.positiveTextColor
    }

    function tokenExpirySummary(account) {
        var tokenExpiry = objectOrEmpty(objectOrEmpty(account).token_expiry)
        if (!hasValue(tokenExpiry.countdown_text))
            return unavailableText
        return hasValue(tokenExpiry.state)
                ? String(tokenExpiry.state) + " · " + String(tokenExpiry.countdown_text)
                : String(tokenExpiry.countdown_text)
    }

    function tokenExpiryDetails(account) {
        var tokenExpiry = objectOrEmpty(objectOrEmpty(account).token_expiry)
        return hasValue(tokenExpiry.expires_at_ms)
                ? qsTr("Expires %1").arg(optionalTime(tokenExpiry.expires_at_ms))
                : unavailableText
    }

    function statusBackground(account) {
        account = objectOrEmpty(account)
        if (account.paused === true)
            return Kirigami.Theme.neutralBackgroundColor
        if (account.healthy !== true || account.warning_level === "critical")
            return Kirigami.Theme.negativeBackgroundColor
        if (account.warning_level === "warning")
            return Kirigami.Theme.neutralBackgroundColor
        return Kirigami.Theme.positiveBackgroundColor
    }

    function warningMessage(account) {
        account = objectOrEmpty(account)
        if (hasValue(account.blocked_reason))
            return String(account.blocked_reason)
        if (hasValue(account.status))
            return String(account.status)
        return qsTr("This account needs attention")
    }

    function isHttpUrl(value) {
        if (!hasValue(value))
            return false
        var normalized = String(value).toLowerCase()
        return normalized.startsWith("https://") || normalized.startsWith("http://")
    }

    function loginIsTerminal() {
        return login.phase === "done" || login.phase === "error"
                || login.phase === "cancelled"
    }

    function loginMessage() {
        if (hasValue(login.message))
            return String(login.message)
        if (login.phase === "starting")
            return qsTr("Starting %1 login…").arg(providerLabel(login.provider))
        if (login.phase === "pending")
            return qsTr("Waiting for %1 sign-in approval…").arg(providerLabel(login.provider))
        if (login.phase === "done")
            return qsTr("Account added")
        if (login.phase === "error")
            return qsTr("Login failed")
        if (login.phase === "cancelled")
            return qsTr("Login cancelled")
        return ""
    }

    function usageReceipts(value) {
        var supported = {
            "login": true,
            "add_account": true,
            "pause_account": true,
            "remove_account": true
        }
        return arrayOrEmpty(value).filter(function(receiptValue) {
            var receipt = objectOrEmpty(receiptValue)
            return supported[receipt.operation] === true
        }).slice(-6).reverse()
    }

    function receiptOperation(receipt) {
        receipt = objectOrEmpty(receipt)
        switch (receipt.operation) {
        case "login": return qsTr("Login")
        case "add_account": return qsTr("Add account")
        case "pause_account": return qsTr("Pause or resume")
        case "remove_account": return qsTr("Remove account")
        default: return qsTr("Account operation")
        }
    }

    function receiptSummary(receipt) {
        receipt = objectOrEmpty(receipt)
        var parts = [receiptOperation(receipt), optionalText(receipt.outcome)]
        if (hasValue(receipt.target_display))
            parts.push(String(receipt.target_display))
        return parts.join(" · ")
    }

    function receiptTiming(receipt) {
        receipt = objectOrEmpty(receipt)
        var started = optionalTime(receipt.started_at_ms)
        var finished = optionalTime(receipt.finished_at_ms)
        return started === unavailableText && finished === unavailableText
                ? unavailableText : started + " → " + finished
    }

    function receiptBackground(receipt) {
        receipt = objectOrEmpty(receipt)
        if (receipt.outcome === "failed")
            return Kirigami.Theme.negativeBackgroundColor
        if (receipt.outcome === "cancelled" || receipt.outcome === "no_change")
            return Kirigami.Theme.neutralBackgroundColor
        return Kirigami.Theme.positiveBackgroundColor
    }

    ColumnLayout {
        id: usageContent
        width: usagePage.availableWidth
        spacing: Kirigami.Units.largeSpacing

        RowLayout {
            Layout.fillWidth: true
            spacing: Kirigami.Units.smallSpacing

            ColumnLayout {
                Layout.fillWidth: true
                spacing: 0

                Kirigami.Heading {
                    text: qsTr("Account usage")
                    level: 1
                }
                Label {
                    Layout.fillWidth: true
                    text: qsTr("Quota, reset, credential health and in-flight state")
                    color: Kirigami.Theme.disabledTextColor
                    elide: Text.ElideRight
                }
            }

            Button {
                text: qsTr("Add account")
                icon.name: "list-add"
                enabled: !usagePage.loginActive
                onClicked: addDialog.open()
            }
            Button {
                text: qsTr("Refresh")
                icon.name: "view-refresh"
                onClicked: usagePage.dispatchRequested(
                    "refresh_requested",
                    JSON.stringify({ "source": "manual" })
                )
            }
        }

        Flow {
            Layout.fillWidth: true
            Layout.preferredHeight: childrenRect.height
            spacing: Kirigami.Units.smallSpacing
            visible: providerCounterRepeater.count > 0

            Repeater {
                id: providerCounterRepeater
                model: usagePage.providerCounters()

                delegate: Rectangle {
                    id: providerCounterChip
                    required property var modelData
                    radius: height / 2
                    implicitWidth: providerCounterLabel.implicitWidth
                            + Kirigami.Units.largeSpacing * 2
                    implicitHeight: providerCounterLabel.implicitHeight
                            + Kirigami.Units.smallSpacing
                    color: Kirigami.Theme.alternateBackgroundColor

                    Label {
                        id: providerCounterLabel
                        anchors.centerIn: parent
                        text: qsTr("%1 · %2 in flight")
                                .arg(usagePage.providerLabel(providerCounterChip.modelData.provider))
                                .arg(providerCounterChip.modelData.count)
                        textFormat: Text.PlainText
                    }
                }
            }
        }

        Kirigami.InlineMessage {
            objectName: "usage-offline-state"
            Layout.fillWidth: true
            visible: usagePage.isOffline
            type: Kirigami.MessageType.Error
            text: usagePage.hasValue(usagePage.connection.error)
                    ? qsTr("Daemon unavailable: %1").arg(usagePage.connection.error)
                    : qsTr("The llmux daemon is offline. The last known account state remains visible.")
        }

        RowLayout {
            Layout.fillWidth: true
            visible: usagePage.isStarting

            BusyIndicator {
                running: parent.visible
            }
            Label {
                Layout.fillWidth: true
                text: qsTr("Connecting to the llmux daemon…")
                color: Kirigami.Theme.disabledTextColor
            }
        }

        Kirigami.AbstractCard {
            objectName: "usage-login-state"
            Layout.fillWidth: true
            visible: usagePage.login.phase !== undefined
                    && usagePage.login.phase !== "idle"

            contentItem: ColumnLayout {
                spacing: Kirigami.Units.smallSpacing

                RowLayout {
                    Layout.fillWidth: true

                    BusyIndicator {
                        running: usagePage.loginActive
                        visible: running
                    }
                    Kirigami.Icon {
                        source: usagePage.providerIcon(usagePage.login.provider)
                        implicitWidth: Kirigami.Units.iconSizes.smallMedium
                        implicitHeight: implicitWidth
                        visible: !usagePage.loginActive
                    }
                    Kirigami.Heading {
                        Layout.fillWidth: true
                        level: 2
                        text: qsTr("%1 login").arg(
                            usagePage.providerLabel(usagePage.login.provider)
                        )
                    }
                    Label {
                        text: usagePage.optionalText(usagePage.login.phase)
                        color: usagePage.login.phase === "error"
                                ? Kirigami.Theme.negativeTextColor
                                : Kirigami.Theme.disabledTextColor
                        textFormat: Text.PlainText
                    }
                }

                Label {
                    Layout.fillWidth: true
                    text: usagePage.loginMessage()
                    wrapMode: Text.Wrap
                    textFormat: Text.PlainText
                    color: usagePage.loginIsTerminal()
                            && usagePage.login.phase === "error"
                            ? Kirigami.Theme.negativeTextColor
                            : Kirigami.Theme.textColor
                }

                ColumnLayout {
                    Layout.fillWidth: true
                    visible: usagePage.login.phase === "pending"
                            && (usagePage.hasValue(usagePage.login.verification_uri)
                                || usagePage.hasValue(usagePage.login.user_code))
                    spacing: Kirigami.Units.smallSpacing

                    Label {
                        text: qsTr("Device verification")
                        font.bold: true
                    }

                    TextField {
                        Layout.fillWidth: true
                        visible: usagePage.hasValue(usagePage.login.verification_uri)
                        text: usagePage.optionalText(usagePage.login.verification_uri)
                        readOnly: true
                        selectByMouse: true
                        Accessible.name: qsTr("Verification URI")
                    }

                    TextField {
                        Layout.fillWidth: true
                        visible: usagePage.hasValue(usagePage.login.user_code)
                        text: usagePage.optionalText(usagePage.login.user_code)
                        readOnly: true
                        selectByMouse: true
                        horizontalAlignment: TextInput.AlignHCenter
                        font.bold: true
                        font.letterSpacing: 2
                        Accessible.name: qsTr("Verification code")
                    }

                    RowLayout {
                        Layout.fillWidth: true

                        Button {
                            text: qsTr("Open verification page")
                            icon.name: "internet-services"
                            enabled: usagePage.isHttpUrl(usagePage.login.verification_uri)
                            onClicked: usagePage.dispatchRequested(
                                "open_url_requested",
                                JSON.stringify({ "url": usagePage.login.verification_uri })
                            )
                        }
                        Button {
                            text: qsTr("Copy code")
                            icon.name: "edit-copy"
                            enabled: usagePage.hasValue(usagePage.login.user_code)
                            onClicked: usagePage.dispatchRequested(
                                "copy_text_requested",
                                JSON.stringify({ "text": usagePage.login.user_code })
                            )
                        }
                        Item { Layout.fillWidth: true }
                    }
                }

                RowLayout {
                    Layout.fillWidth: true
                    visible: usagePage.loginActive

                    Item { Layout.fillWidth: true }
                    Button {
                        text: usagePage.loginCancelling
                            ? qsTr("Cancelling…") : qsTr("Cancel login")
                        icon.name: "dialog-cancel"
                        enabled: usagePage.login.phase === "pending"
                            && !usagePage.loginCancelling
                        onClicked: usagePage.dispatchRequested(
                            "login_cancelled",
                            "{}"
                        )
                    }
                }
            }
        }

        ColumnLayout {
            objectName: "usage-empty-state"
            Layout.fillWidth: true
            Layout.topMargin: Kirigami.Units.gridUnit * 2
            visible: usagePage.accounts.length === 0
                    && !usagePage.isStarting && !usagePage.isOffline
            spacing: Kirigami.Units.smallSpacing

            Kirigami.Icon {
                Layout.alignment: Qt.AlignHCenter
                source: "user-identity"
                implicitWidth: Kirigami.Units.iconSizes.huge
                implicitHeight: implicitWidth
                color: Kirigami.Theme.disabledTextColor
            }
            Kirigami.Heading {
                Layout.alignment: Qt.AlignHCenter
                text: qsTr("No accounts yet")
                level: 2
            }
            Label {
                Layout.alignment: Qt.AlignHCenter
                text: qsTr("Add Claude, Codex, Grok, or an Anthropic API key.")
                color: Kirigami.Theme.disabledTextColor
            }
            Button {
                Layout.alignment: Qt.AlignHCenter
                text: qsTr("Add account")
                icon.name: "list-add"
                onClicked: addDialog.open()
            }
        }

        GridLayout {
            Layout.fillWidth: true
            columns: usagePage.width >= 760 ? 2 : 1
            columnSpacing: Kirigami.Units.largeSpacing
            rowSpacing: Kirigami.Units.largeSpacing

            Repeater {
                model: usagePage.accounts

                delegate: Kirigami.AbstractCard {
                    id: accountCard
                    required property var modelData
                    readonly property var account: usagePage.objectOrEmpty(modelData)
                    readonly property var tokenExpiry: usagePage.objectOrEmpty(account.token_expiry)
                    readonly property var gauges: usagePage.visibleGauges(account)
                    readonly property bool busy: usagePage.hasValue(account.busy_action)

                    Layout.fillWidth: true
                    opacity: account.paused === true ? 0.72 : 1

                    contentItem: ColumnLayout {
                        spacing: Kirigami.Units.smallSpacing

                        RowLayout {
                            Layout.fillWidth: true

                            Kirigami.Icon {
                                source: usagePage.providerIcon(accountCard.account.provider)
                                implicitWidth: Kirigami.Units.iconSizes.medium
                                implicitHeight: implicitWidth
                            }
                            ColumnLayout {
                                Layout.fillWidth: true
                                spacing: 0

                                RowLayout {
                                    Layout.fillWidth: true
                                    Kirigami.Heading {
                                        text: usagePage.providerLabel(accountCard.account.provider)
                                        level: 2
                                    }
                                    Label {
                                        visible: usagePage.accountIsCurrent(accountCard.account)
                                        text: qsTr("Current")
                                        color: Kirigami.Theme.highlightedTextColor
                                        font.bold: true
                                    }
                                    Item { Layout.fillWidth: true }
                                }
                                Label {
                                    Layout.fillWidth: true
                                    text: usagePage.accountDisplay(accountCard.account)
                                    textFormat: Text.PlainText
                                    color: Kirigami.Theme.disabledTextColor
                                    elide: Text.ElideMiddle
                                }
                            }

                            ToolButton {
                                id: accountActionsButton
                                icon.name: "overflow-menu"
                                text: qsTr("Account actions")
                                display: AbstractButton.IconOnly
                                enabled: !accountCard.busy
                                        && usagePage.hasValue(accountCard.account.id)
                                onClicked: accountActions.open()
                                ToolTip.visible: hovered
                                ToolTip.text: text

                                Menu {
                                    id: accountActions

                                    MenuItem {
                                        text: accountCard.account.paused === true
                                                ? qsTr("Resume") : qsTr("Pause")
                                        icon.name: accountCard.account.paused === true
                                                ? "media-playback-start" : "media-playback-pause"
                                        enabled: !accountCard.busy
                                        onTriggered: usagePage.dispatchRequested(
                                            "pause_account_requested",
                                            JSON.stringify({
                                                "account": accountCard.account.id,
                                                "paused": accountCard.account.paused !== true
                                            })
                                        )
                                    }
                                    MenuSeparator {}
                                    MenuItem {
                                        text: qsTr("Remove…")
                                        icon.name: "edit-delete"
                                        enabled: !accountCard.busy
                                        onTriggered: {
                                            usagePage.removeCandidate = accountCard.account
                                            removeDialog.open()
                                        }
                                    }
                                }
                            }
                        }

                        Flow {
                            Layout.fillWidth: true
                            Layout.preferredHeight: childrenRect.height
                            spacing: Kirigami.Units.smallSpacing

                            Rectangle {
                                radius: height / 2
                                implicitWidth: accountStatusLabel.implicitWidth
                                        + Kirigami.Units.largeSpacing
                                implicitHeight: accountStatusLabel.implicitHeight
                                        + Kirigami.Units.smallSpacing
                                color: usagePage.statusBackground(accountCard.account)

                                Label {
                                    id: accountStatusLabel
                                    anchors.centerIn: parent
                                    text: usagePage.optionalText(accountCard.account.status)
                                    textFormat: Text.PlainText
                                }
                            }
                            Label {
                                visible: accountCard.account.paused === true
                                text: qsTr("Paused")
                                font.bold: true
                            }
                            Label {
                                text: accountCard.account.healthy === true
                                        ? qsTr("Healthy") : qsTr("Needs attention")
                                color: accountCard.account.healthy === true
                                        ? Kirigami.Theme.positiveTextColor
                                        : Kirigami.Theme.negativeTextColor
                            }
                            Label {
                                text: usagePage.hasValue(accountCard.account.in_flight)
                                        ? qsTr("%1 in flight").arg(accountCard.account.in_flight)
                                        : qsTr("In flight: %1").arg(usagePage.unavailableText)
                                color: Kirigami.Theme.disabledTextColor
                            }
                        }

                        Kirigami.InlineMessage {
                            Layout.fillWidth: true
                            visible: accountCard.account.warning_level === "warning"
                                    || accountCard.account.warning_level === "critical"
                                    || usagePage.hasValue(accountCard.account.blocked_reason)
                            type: accountCard.account.warning_level === "critical"
                                    ? Kirigami.MessageType.Error
                                    : Kirigami.MessageType.Warning
                            text: usagePage.warningMessage(accountCard.account)
                        }

                        RowLayout {
                            Layout.fillWidth: true

                            Kirigami.Icon {
                                source: "appointment-soon"
                                implicitWidth: Kirigami.Units.iconSizes.small
                                implicitHeight: implicitWidth
                            }
                            Label {
                                text: qsTr("Token")
                                font.bold: true
                            }
                            Label {
                                Layout.fillWidth: true
                                text: usagePage.tokenExpirySummary(accountCard.account)
                                textFormat: Text.PlainText
                                color: accountCard.tokenExpiry.state === "expired"
                                        ? Kirigami.Theme.negativeTextColor
                                        : Kirigami.Theme.disabledTextColor
                                elide: Text.ElideRight
                                ToolTip.visible: tokenExpiryHover.hovered
                                ToolTip.text: usagePage.tokenExpiryDetails(accountCard.account)

                                HoverHandler {
                                    id: tokenExpiryHover
                                }
                            }
                        }

                        Repeater {
                            model: accountCard.gauges

                            delegate: ColumnLayout {
                                id: gaugeRow
                                required property var modelData
                                readonly property var gauge: usagePage.objectOrEmpty(modelData)
                                Layout.fillWidth: true
                                spacing: 0

                                Component.onCompleted: usagePage.renderedGaugeRows += 1
                                Component.onDestruction: usagePage.renderedGaugeRows = Math.max(
                                    0, usagePage.renderedGaugeRows - 1
                                )

                                RowLayout {
                                    Layout.fillWidth: true
                                    Label {
                                        text: usagePage.gaugeLabel(gaugeRow.gauge)
                                        font.bold: true
                                    }
                                    Label {
                                        visible: gaugeRow.gauge.constraining === true
                                        text: qsTr("Constraining")
                                        color: Kirigami.Theme.negativeTextColor
                                        font.bold: true
                                    }
                                    Item { Layout.fillWidth: true }
                                    Label {
                                        text: usagePage.gaugeRemaining(gaugeRow.gauge)
                                        color: usagePage.gaugeColor(
                                            gaugeRow.gauge,
                                            accountCard.account
                                        )
                                    }
                                }

                                ProgressBar {
                                    Layout.fillWidth: true
                                    from: 0
                                    to: 1
                                    value: gaugeRow.gauge.available === true
                                            ? usagePage.clampFraction(gaugeRow.gauge.remaining_fraction)
                                            : 0
                                    indeterminate: gaugeRow.gauge.available !== true
                                    Accessible.name: qsTr("%1 quota").arg(
                                        usagePage.gaugeLabel(gaugeRow.gauge)
                                    )
                                    Accessible.description: usagePage.gaugeRemaining(gaugeRow.gauge)
                                }

                                RowLayout {
                                    Layout.fillWidth: true
                                    Label {
                                        text: usagePage.gaugeUsed(gaugeRow.gauge)
                                        color: Kirigami.Theme.disabledTextColor
                                    }
                                    Item { Layout.fillWidth: true }
                                    Label {
                                        text: usagePage.gaugeReset(gaugeRow.gauge)
                                        textFormat: Text.PlainText
                                        color: Kirigami.Theme.disabledTextColor
                                    }
                                }
                            }
                        }

                        RowLayout {
                            Layout.fillWidth: true
                            visible: accountCard.busy

                            BusyIndicator {
                                running: parent.visible
                                implicitWidth: Kirigami.Units.iconSizes.small
                                implicitHeight: implicitWidth
                            }
                            Label {
                                Layout.fillWidth: true
                                text: qsTr("Working: %1").arg(
                                    usagePage.optionalText(accountCard.account.busy_action)
                                )
                                textFormat: Text.PlainText
                                color: Kirigami.Theme.disabledTextColor
                            }
                        }
                    }
                }
            }
        }

        ColumnLayout {
            id: verificationReceiptSection
            objectName: "usage-verification-receipts"
            Layout.fillWidth: true
            visible: usagePage.verificationReceipts.length > 0
            spacing: Kirigami.Units.smallSpacing

            Kirigami.Separator { Layout.fillWidth: true }
            Kirigami.Heading {
                text: qsTr("Recent account results")
                level: 2
            }

            Repeater {
                model: usagePage.verificationReceipts

                delegate: Kirigami.AbstractCard {
                    id: verificationReceiptCard
                    required property var modelData
                    readonly property var receipt: usagePage.objectOrEmpty(modelData)
                    Layout.fillWidth: true

                    contentItem: ColumnLayout {
                        spacing: 0

                        RowLayout {
                            Layout.fillWidth: true
                            Rectangle {
                                radius: height / 2
                                implicitWidth: receiptOutcome.implicitWidth
                                        + Kirigami.Units.largeSpacing
                                implicitHeight: receiptOutcome.implicitHeight
                                        + Kirigami.Units.smallSpacing
                                color: usagePage.receiptBackground(verificationReceiptCard.receipt)

                                Label {
                                    id: receiptOutcome
                                    anchors.centerIn: parent
                                    text: usagePage.optionalText(verificationReceiptCard.receipt.outcome)
                                    textFormat: Text.PlainText
                                }
                            }
                            Label {
                                Layout.fillWidth: true
                                text: usagePage.receiptSummary(verificationReceiptCard.receipt)
                                textFormat: Text.PlainText
                                font.bold: true
                                elide: Text.ElideRight
                            }
                            Label {
                                text: usagePage.receiptTiming(verificationReceiptCard.receipt)
                                textFormat: Text.PlainText
                                color: Kirigami.Theme.disabledTextColor
                            }
                        }
                        Label {
                            Layout.fillWidth: true
                            text: usagePage.optionalText(verificationReceiptCard.receipt.message)
                            textFormat: Text.PlainText
                            wrapMode: Text.Wrap
                            color: Kirigami.Theme.disabledTextColor
                        }
                    }
                }
            }
        }
    }

    Dialog {
        id: removeDialog
        objectName: "remove-account-confirmation"
        modal: true
        width: Math.min(480, Math.max(300, usagePage.width - Kirigami.Units.gridUnit * 4))
        title: qsTr("Remove account?")
        standardButtons: Dialog.Cancel | Dialog.Ok

        onOpened: standardButton(Dialog.Ok).text = qsTr("Remove")
        onAccepted: {
            var candidate = usagePage.objectOrEmpty(usagePage.removeCandidate)
            if (usagePage.hasValue(candidate.id)) {
                usagePage.dispatchRequested(
                    "remove_account_confirmed",
                    JSON.stringify({ "account": candidate.id })
                )
            }
            usagePage.removeCandidate = ({})
        }
        onRejected: usagePage.removeCandidate = ({})

        contentItem: ColumnLayout {
            Label {
                Layout.fillWidth: true
                text: qsTr("Remove %1 from llmux? This cannot be undone.").arg(
                    usagePage.accountDisplay(usagePage.removeCandidate)
                )
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
            }
        }
    }

    Dialog {
        id: addDialog
        objectName: "add-account-dialog"
        modal: true
        width: Math.min(520, Math.max(320, usagePage.width - Kirigami.Units.gridUnit * 4))
        title: qsTr("Add account")
        closePolicy: Popup.CloseOnEscape | Popup.CloseOnPressOutside
        property var providers: [
            {
                "key": "claude",
                "label": qsTr("Claude"),
                "description": qsTr("Sign in with Claude OAuth in your browser.")
            },
            {
                "key": "codex",
                "label": qsTr("Codex"),
                "description": qsTr("Sign in with your ChatGPT subscription.")
            },
            {
                "key": "grok",
                "label": qsTr("Grok"),
                "description": qsTr("Use the device verification link and code.")
            },
            {
                "key": "api",
                "label": qsTr("API key"),
                "description": qsTr("Add an Anthropic API key directly.")
            }
        ]
        readonly property var selectedProvider: providers[Math.max(0, providerSelector.currentIndex)]
        readonly property bool apiMode: selectedProvider.key === "api"

        function submit() {
            if (apiMode) {
                usagePage.dispatchRequested(
                    "add_api_key_submitted",
                    JSON.stringify({
                        "name": accountNameField.text.trim() === ""
                                ? null : accountNameField.text.trim(),
                        "api_key": apiKeyField.text
                    })
                )
                apiKeyField.text = ""
            } else {
                usagePage.dispatchRequested(
                    "login_started",
                    JSON.stringify({ "provider": selectedProvider.key })
                )
            }
            close()
        }

        onOpened: {
            providerSelector.currentIndex = 0
            accountNameField.clear()
            apiKeyField.clear()
        }
        onClosed: {
            accountNameField.clear()
            apiKeyField.clear()
        }

        contentItem: ColumnLayout {
            spacing: Kirigami.Units.largeSpacing

            ComboBox {
                id: providerSelector
                Layout.fillWidth: true
                model: addDialog.providers
                textRole: "label"
                Accessible.name: qsTr("Account provider")
            }

            Label {
                Layout.fillWidth: true
                text: addDialog.selectedProvider.description
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
                color: Kirigami.Theme.disabledTextColor
            }

            TextField {
                id: accountNameField
                Layout.fillWidth: true
                visible: addDialog.apiMode
                placeholderText: qsTr("Account label (optional)")
                maximumLength: 120
            }

            TextField {
                id: apiKeyField
                Layout.fillWidth: true
                visible: addDialog.apiMode
                placeholderText: qsTr("Anthropic API key")
                echoMode: TextInput.Password
                inputMethodHints: Qt.ImhSensitiveData | Qt.ImhNoPredictiveText
                passwordCharacter: "•"
                maximumLength: 4096
                onAccepted: {
                    if (addButton.enabled)
                        addDialog.submit()
                }
            }

            RowLayout {
                Layout.fillWidth: true

                Item { Layout.fillWidth: true }
                Button {
                    text: qsTr("Cancel")
                    onClicked: addDialog.close()
                }
                Button {
                    id: addButton
                    text: addDialog.apiMode ? qsTr("Add") : qsTr("Continue")
                    icon.name: addDialog.apiMode ? "list-add" : "go-next"
                    enabled: !addDialog.apiMode || apiKeyField.length > 0
                    highlighted: true
                    onClicked: addDialog.submit()
                }
            }
        }
    }
}
