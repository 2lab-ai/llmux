pragma ComponentBehavior: Bound

import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

Kirigami.ScrollablePage {
    id: usagePage

    padding: IslandTheme.pagePadding
    palette.window: IslandTheme.panel
    palette.windowText: IslandTheme.primaryText
    palette.text: IslandTheme.primaryText
    palette.buttonText: IslandTheme.primaryText
    palette.base: IslandTheme.field
    palette.highlight: IslandTheme.primaryText
    palette.highlightedText: IslandTheme.panel
    background: Rectangle { color: IslandTheme.panel }

    property var uiState: ({})
    property var removeCandidate: ({})
    property bool advancedVisible: false
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

    function primaryGauges(account) {
        var gauges = visibleGauges(account)
        if (gauges.length === 0)
            return []
        for (var index = 0; index < gauges.length; index += 1) {
            if (objectOrEmpty(gauges[index]).constraining === true)
                return [gauges[index]]
        }
        for (var fiveHourIndex = 0; fiveHourIndex < gauges.length; fiveHourIndex += 1) {
            if (objectOrEmpty(gauges[fiveHourIndex]).kind === "five_hour")
                return [gauges[fiveHourIndex]]
        }
        return [gauges[0]]
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
        return IslandTheme.quotaAccent(
            clampFraction(gauge.remaining_fraction),
            gauge.constraining,
            account.warning_level
        )
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
            return IslandTheme.amberTint
        if (account.healthy !== true || account.warning_level === "critical")
            return IslandTheme.redTint
        if (account.warning_level === "warning")
            return IslandTheme.amberTint
        return IslandTheme.surfaceRaised
    }

    function warningMessage(account) {
        account = objectOrEmpty(account)
        if (hasValue(account.blocked_reason))
            return String(account.blocked_reason)
        var tokenExpiry = objectOrEmpty(account.token_expiry)
        if (tokenExpiry.state === "expired")
            return qsTr("Authentication required: this credential has expired")
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

    function visibleUsageReceipts() {
        if (advancedVisible)
            return verificationReceipts
        return verificationReceipts.filter(function(receiptValue) {
            return objectOrEmpty(receiptValue).outcome === "failed"
        })
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
            return IslandTheme.redTint
        if (receipt.outcome === "cancelled" || receipt.outcome === "no_change")
            return IslandTheme.amberTint
        return IslandTheme.greenTint
    }

    ColumnLayout {
        id: usageContent
        width: usagePage.availableWidth
        spacing: IslandTheme.sectionSpacing

        RowLayout {
            Layout.fillWidth: true
            spacing: IslandTheme.spaceSm

            ColumnLayout {
                Layout.fillWidth: true
                spacing: 0

                Label {
                    text: qsTr("Usage")
                    color: IslandTheme.primaryText
                    font.pixelSize: 20
                    font.weight: Font.DemiBold
                }
                Label {
                    Layout.fillWidth: true
                    text: qsTr("Account status and remaining quota")
                    color: IslandTheme.secondaryText
                    font.pixelSize: 11
                    elide: Text.ElideRight
                }
            }

            IslandButton {
                text: qsTr("Add account")
                icon.name: "list-add"
                highlighted: true
                enabled: !usagePage.loginActive
                onClicked: addDialog.open()
            }
            IslandButton {
                text: qsTr("Refresh")
                icon.name: "view-refresh"
                onClicked: usagePage.dispatchRequested(
                    "refresh_requested",
                    JSON.stringify({ "source": "manual" })
                )
            }
            IslandButton {
                objectName: "usage-advanced-disclosure"
                text: qsTr("Advanced")
                checkable: true
                checked: usagePage.advancedVisible
                Accessible.name: qsTr("Show advanced usage details")
                onClicked: usagePage.advancedVisible = checked
            }
        }

        Flow {
            id: providerCounterFlow
            Layout.fillWidth: true
            Layout.preferredHeight: childrenRect.height
            spacing: IslandTheme.iconTextGap
            visible: usagePage.advancedVisible && providerCounterRepeater.count > 0

            Repeater {
                id: providerCounterRepeater
                model: usagePage.providerCounters()

                delegate: Rectangle {
                    id: providerCounterChip
                    required property var modelData
                    radius: 0
                    implicitWidth: providerCounterLabel.implicitWidth
                            + IslandTheme.chipPaddingX * 2
                    implicitHeight: providerCounterLabel.implicitHeight
                            + IslandTheme.chipPaddingY * 2
                    color: IslandTheme.surface
                    border.color: IslandTheme.border
                    border.width: 1

                    Label {
                        id: providerCounterLabel
                        anchors.centerIn: parent
                        text: qsTr("%1 · %2 in flight")
                                .arg(usagePage.providerLabel(providerCounterChip.modelData.provider))
                                .arg(providerCounterChip.modelData.count)
                        textFormat: Text.PlainText
                        color: IslandTheme.secondaryText
                        font.family: IslandTheme.monoFamily
                        font.pixelSize: 10
                    }
                }
            }
        }

        IslandInlineMessage {
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
                color: IslandTheme.secondaryText
            }
        }

        IslandCard {
            objectName: "usage-login-state"
            Layout.fillWidth: true
            visible: usagePage.login.phase !== undefined
                    && usagePage.login.phase !== "idle"

            contentItem: ColumnLayout {
                spacing: IslandTheme.spaceSm

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
                    Label {
                        Layout.fillWidth: true
                        text: qsTr("%1 login").arg(
                            usagePage.providerLabel(usagePage.login.provider)
                        )
                        color: IslandTheme.primaryText
                        font.pixelSize: 14
                        font.weight: Font.DemiBold
                    }
                    Label {
                        text: usagePage.optionalText(usagePage.login.phase)
                        color: usagePage.login.phase === "error"
                                ? IslandTheme.red
                                : IslandTheme.secondaryText
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
                            ? IslandTheme.red
                            : IslandTheme.primaryText
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

                    IslandTextField {
                        Layout.fillWidth: true
                        visible: usagePage.hasValue(usagePage.login.verification_uri)
                        text: usagePage.optionalText(usagePage.login.verification_uri)
                        readOnly: true
                        selectByMouse: true
                        Accessible.name: qsTr("Verification URI")
                    }

                    IslandTextField {
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
                        objectName: "usage-verification-actions"
                        spacing: IslandTheme.spaceSm
                        Layout.fillWidth: true

                        IslandButton {
                            text: qsTr("Open verification page")
                            icon.name: "internet-services"
                            enabled: usagePage.isHttpUrl(usagePage.login.verification_uri)
                            onClicked: usagePage.dispatchRequested(
                                "open_url_requested",
                                JSON.stringify({ "url": usagePage.login.verification_uri })
                            )
                        }
                        IslandButton {
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
                    spacing: IslandTheme.spaceSm
                    Layout.fillWidth: true
                    visible: usagePage.loginActive

                    Item { Layout.fillWidth: true }
                    IslandButton {
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
                color: IslandTheme.disabledText
            }
            Label {
                Layout.alignment: Qt.AlignHCenter
                text: qsTr("No accounts yet")
                color: IslandTheme.primaryText
                font.pixelSize: 15
                font.weight: Font.DemiBold
            }
            Label {
                Layout.alignment: Qt.AlignHCenter
                text: qsTr("Add Claude, Codex, Grok, or an Anthropic API key.")
                color: IslandTheme.secondaryText
            }
            IslandButton {
                Layout.alignment: Qt.AlignHCenter
                text: qsTr("Add account")
                icon.name: "list-add"
                onClicked: addDialog.open()
            }
        }

        GridLayout {
            Layout.fillWidth: true
            columns: usagePage.width >= 760 ? 2 : 1
            uniformCellWidths: true
            columnSpacing: IslandTheme.peerGap
            rowSpacing: IslandTheme.peerGap

            Repeater {
                model: usagePage.accounts

                delegate: IslandCard {
                    id: accountCard
                    required property var modelData
                    readonly property var account: usagePage.objectOrEmpty(modelData)
                    readonly property var tokenExpiry: usagePage.objectOrEmpty(account.token_expiry)
                    readonly property var gauges: usagePage.visibleGauges(account)
                    readonly property bool busy: usagePage.hasValue(account.busy_action)

                    Layout.fillWidth: true
                    opacity: account.paused === true ? 0.72 : 1
                    interactive: true
                    strokeColor: usagePage.accountIsCurrent(accountCard.account)
                        ? IslandTheme.borderStrong
                        : IslandTheme.border

                    contentItem: ColumnLayout {
                        spacing: IslandTheme.spaceSm

                        RowLayout {
                            spacing: IslandTheme.spaceSm
                            Layout.fillWidth: true

                            Kirigami.Icon {
                                source: usagePage.providerIcon(accountCard.account.provider)
                                implicitWidth: Kirigami.Units.iconSizes.medium
                                implicitHeight: implicitWidth
                                color: IslandTheme.providerAccent(accountCard.account.provider)
                            }
                            ColumnLayout {
                                Layout.fillWidth: true
                                spacing: 0

                                RowLayout {
                                    Layout.fillWidth: true
                                    Label {
                                        text: usagePage.providerLabel(accountCard.account.provider)
                                        color: IslandTheme.primaryText
                                        font.pixelSize: 14
                                        font.weight: Font.DemiBold
                                    }
                                    Label {
                                        visible: usagePage.accountIsCurrent(accountCard.account)
                                        text: qsTr("Current")
                                        color: IslandTheme.secondaryText
                                        font.bold: true
                                        font.pixelSize: 10
                                    }
                                    Item { Layout.fillWidth: true }
                                }
                                Label {
                                    Layout.fillWidth: true
                                    text: usagePage.accountDisplay(accountCard.account)
                                    textFormat: Text.PlainText
                                    color: IslandTheme.secondaryText
                                    font.family: IslandTheme.monoFamily
                                    font.pixelSize: 10
                                    elide: Text.ElideMiddle
                                }
                            }

                            IslandButton {
                                id: accountActionsButton
                                visible: usagePage.advancedVisible
                                text: "⋯"
                                display: AbstractButton.TextOnly
                                Accessible.name: qsTr("Account actions")
                                enabled: !accountCard.busy
                                        && usagePage.hasValue(accountCard.account.id)
                                onClicked: accountActions.open()
                                ToolTip.visible: hovered
                                ToolTip.text: Accessible.name

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
                            spacing: IslandTheme.spaceSm

                            Rectangle {
                                radius: 0
                                implicitWidth: accountStatusLabel.implicitWidth
                                        + IslandTheme.chipPaddingX * 2
                                implicitHeight: accountStatusLabel.implicitHeight
                                        + IslandTheme.chipPaddingY * 2
                                color: usagePage.statusBackground(accountCard.account)

                                Label {
                                    id: accountStatusLabel
                                    anchors.centerIn: parent
                                    text: usagePage.optionalText(accountCard.account.status)
                                    textFormat: Text.PlainText
                                    color: accountCard.account.healthy === true
                                        ? IslandTheme.primaryText : IslandTheme.amber
                                    font.pixelSize: 10
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
                                        ? IslandTheme.secondaryText
                                        : IslandTheme.red
                            }
                            Label {
                                visible: !usagePage.advancedVisible
                                    && usagePage.hasValue(accountCard.account.in_flight)
                                    && Number(accountCard.account.in_flight) > 0
                                text: qsTr("Active")
                                color: IslandTheme.primaryText
                            }
                            Label {
                                visible: usagePage.advancedVisible
                                text: usagePage.hasValue(accountCard.account.in_flight)
                                        ? qsTr("%1 in flight").arg(accountCard.account.in_flight)
                                        : qsTr("In flight: %1").arg(usagePage.unavailableText)
                                color: IslandTheme.secondaryText
                                font.family: IslandTheme.monoFamily
                            }
                        }

                        IslandInlineMessage {
                            Layout.fillWidth: true
                            visible: accountCard.account.warning_level === "warning"
                                    || accountCard.account.warning_level === "critical"
                                    || usagePage.hasValue(accountCard.account.blocked_reason)
                                    || accountCard.account.healthy === false
                                    || accountCard.tokenExpiry.state === "expired"
                            type: accountCard.account.warning_level === "critical"
                                    || accountCard.account.healthy === false
                                    || accountCard.tokenExpiry.state === "expired"
                                    ? Kirigami.MessageType.Error
                                    : Kirigami.MessageType.Warning
                            text: usagePage.warningMessage(accountCard.account)
                        }

                        RowLayout {
                            Layout.fillWidth: true
                            visible: usagePage.advancedVisible

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
                                        ? IslandTheme.red
                                        : IslandTheme.secondaryText
                                elide: Text.ElideRight
                                ToolTip.visible: tokenExpiryHover.hovered
                                ToolTip.text: usagePage.tokenExpiryDetails(accountCard.account)

                                HoverHandler {
                                    id: tokenExpiryHover
                                }
                            }
                        }

                        Repeater {
                            model: usagePage.advancedVisible
                                ? accountCard.gauges
                                : usagePage.primaryGauges(accountCard.account)

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
                                        font.family: IslandTheme.monoFamily
                                        font.pixelSize: 10
                                    }
                                    Label {
                                        visible: gaugeRow.gauge.constraining === true
                                        text: qsTr("Constraining")
                                        color: IslandTheme.red
                                        font.bold: true
                                        font.pixelSize: 9
                                    }
                                    Item { Layout.fillWidth: true }
                                    Label {
                                        text: usagePage.gaugeRemaining(gaugeRow.gauge)
                                        color: usagePage.gaugeColor(
                                            gaugeRow.gauge,
                                            accountCard.account
                                        )
                                        font.family: IslandTheme.monoFamily
                                        font.pixelSize: 10
                                    }
                                }

                                IslandProgressBar {
                                    Layout.fillWidth: true
                                    from: 0
                                    to: 1
                                    value: gaugeRow.gauge.available === true
                                            ? usagePage.clampFraction(gaugeRow.gauge.remaining_fraction)
                                            : 0
                                    accentColor: usagePage.gaugeColor(
                                        gaugeRow.gauge,
                                        accountCard.account
                                    )
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
                                        color: IslandTheme.secondaryText
                                        font.family: IslandTheme.monoFamily
                                    }
                                    Item { Layout.fillWidth: true }
                                    Label {
                                        text: usagePage.gaugeReset(gaugeRow.gauge)
                                        textFormat: Text.PlainText
                                        color: IslandTheme.secondaryText
                                        font.family: IslandTheme.monoFamily
                                    }
                                }
                            }
                        }

                        Label {
                            Layout.fillWidth: true
                            visible: usagePage.primaryGauges(accountCard.account).length === 0
                            text: qsTr("Quota data is unavailable")
                            color: IslandTheme.secondaryText
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
                                color: IslandTheme.secondaryText
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
            visible: usagePage.visibleUsageReceipts().length > 0
            spacing: Kirigami.Units.smallSpacing

            IslandSeparator { Layout.fillWidth: true }
            IslandSectionLabel {
                text: qsTr("Recent account results")
            }

            Repeater {
                model: usagePage.visibleUsageReceipts()

                delegate: IslandCard {
                    id: verificationReceiptCard
                    required property var modelData
                    readonly property var receipt: usagePage.objectOrEmpty(modelData)
                    Layout.fillWidth: true

                    contentItem: ColumnLayout {
                        spacing: 0

                        RowLayout {
                            Layout.fillWidth: true
                            Rectangle {
                                radius: 0
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
                                font.pixelSize: 11
                                elide: Text.ElideRight
                            }
                            Label {
                                text: usagePage.receiptTiming(verificationReceiptCard.receipt)
                                textFormat: Text.PlainText
                                color: IslandTheme.secondaryText
                                font.family: IslandTheme.monoFamily
                                font.pixelSize: 10
                            }
                        }
                        Label {
                            Layout.fillWidth: true
                            text: usagePage.optionalText(verificationReceiptCard.receipt.message)
                            textFormat: Text.PlainText
                            wrapMode: Text.Wrap
                            color: IslandTheme.secondaryText
                            font.pixelSize: 10
                        }
                    }
                }
            }
        }
    }

    IslandDialog {
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

    IslandDialog {
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

            IslandSegmentedControl {
                id: providerSelector
                Layout.fillWidth: true
                model: addDialog.providers.map(function(provider) { return provider.label })
                onActivated: function(index) { currentIndex = index }
                Accessible.name: qsTr("Account provider")
            }

            Label {
                Layout.fillWidth: true
                text: addDialog.selectedProvider.description
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
                color: IslandTheme.secondaryText
            }

            IslandTextField {
                id: accountNameField
                Layout.fillWidth: true
                visible: addDialog.apiMode
                placeholderText: qsTr("Account label (optional)")
                maximumLength: 120
            }

            IslandTextField {
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
                objectName: "usage-add-dialog-actions"
                spacing: IslandTheme.spaceSm
                Layout.fillWidth: true

                Item { Layout.fillWidth: true }
                IslandButton {
                    text: qsTr("Cancel")
                    onClicked: addDialog.close()
                }
                IslandButton {
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
