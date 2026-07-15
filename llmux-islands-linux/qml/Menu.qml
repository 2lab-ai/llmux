pragma ComponentBehavior: Bound

import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

Kirigami.ScrollablePage {
    id: menuPage
    objectName: "menu-surface"

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
    // Kept as shell-facing compatibility inputs while canonical state remains authoritative.
    property string surfaceMode: ""
    property bool autostartEnabled: false
    property bool advancedVisible: false
    signal dispatchRequested(string action, string payloadJson)

    readonly property string unavailableText: qsTr("Unavailable")
    readonly property var settings: objectOrEmpty(uiState.settings)
    readonly property var connection: objectOrEmpty(uiState.connection)
    readonly property var operation: objectOrEmpty(uiState.operation)
    readonly property var windowState: objectOrEmpty(uiState.window)
    readonly property var screens: arrayOrEmpty(settings.screens)
    readonly property var sounds: arrayOrEmpty(settings.sounds)
    readonly property var events: arrayOrEmpty(settings.events)
    readonly property var autostart: objectOrEmpty(settings.autostart)
    readonly property var maintenance: objectOrEmpty(settings.maintenance)
    readonly property var capabilities: objectOrEmpty(settings.capabilities)
    readonly property var verificationReceipts: arrayOrEmpty(uiState.verification_receipts)
    readonly property var menuReceiptItems: receiptsForMenu(verificationReceipts)
    readonly property var visibleMenuReceiptItems: advancedVisible
        ? menuReceiptItems : failedReceipts(menuReceiptItems)
    readonly property string currentChannel: optionalText(maintenance.channel)
    readonly property string aboutIslandsVersion: optionalText(maintenance.islands_version)
    readonly property string aboutDaemonVersion: optionalText(connection.daemon_version)
    readonly property string aboutLicense: optionalText(maintenance.license)
    readonly property string aboutSourceUrl: optionalText(maintenance.source_url)
    readonly property string aboutReleasesUrl: hasValue(maintenance.source_url)
        ? String(maintenance.source_url).replace(/\/$/, "") + "/releases" : ""
    readonly property real preferredContentHeight: menuContent.implicitHeight

    property string connectionValidationMessage: ""
    property string connectionSchemeDraft: endpointScheme(connection.endpoint_display)
    property bool clearApiKeyRequested: false
    property string pendingChannel: ""
    property var pendingEvent: ({})

    title: qsTr("Menu")

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
            && typeof value === "object" && arrayLikeLength(value) < 0 ? value : {}
    }

    function hasOwn(value, key) {
        return value !== null
            && typeof value === "object"
            && Object.prototype.hasOwnProperty.call(value, key)
    }

    function hasValue(value) {
        return value !== undefined && value !== null && String(value).length > 0
    }

    function optionalText(value) {
        return hasValue(value) ? String(value) : ""
    }

    function displayText(value) {
        var text = optionalText(value)
        return text.length > 0 ? text : unavailableText
    }

    function optionId(option, index) {
        var item = objectOrEmpty(option)
        if (hasValue(item.id))
            return String(item.id)
        if (hasValue(item.key))
            return String(item.key)
        if (hasValue(item.value))
            return String(item.value)
        return ""
    }

    function optionLabel(option, index) {
        var item = objectOrEmpty(option)
        if (hasValue(item.label))
            return String(item.label)
        if (hasValue(item.name))
            return String(item.name)
        var id = optionId(option, index)
        return id.length > 0 ? id : unavailableText
    }

    function selectedOptionIndex(options, explicitId) {
        var selectedId = optionalText(explicitId)
        for (var index = 0; index < options.length; index += 1) {
            var option = objectOrEmpty(options[index])
            if (selectedId.length > 0 && optionId(option, index) === selectedId)
                return index
            if (selectedId.length === 0 && option.selected === true)
                return index
        }
        return -1
    }

    function isOperationBusy(kind) {
        return optionalText(operation.id).length > 0
            && optionalText(operation.kind) === kind
    }

    function endpointHost(endpoint) {
        var value = optionalText(endpoint).trim()
        var scheme = value.indexOf("://")
        if (scheme >= 0)
            value = value.substring(scheme + 3)
        var terminator = value.search(/[/?#]/)
        if (terminator >= 0)
            value = value.substring(0, terminator)
        if (value.charAt(0) === "[") {
            var bracket = value.indexOf("]")
            return bracket > 0 ? value.substring(1, bracket) : ""
        }
        var firstColon = value.indexOf(":")
        var lastColon = value.lastIndexOf(":")
        return firstColon > 0 && firstColon === lastColon
            ? value.substring(0, firstColon) : value
    }

    function endpointScheme(endpoint) {
        var value = optionalText(endpoint).trim().toLowerCase()
        return value.indexOf("https://") === 0 ? "https" : "http"
    }

    function endpointPort(endpoint) {
        var value = optionalText(endpoint).trim()
        var scheme = value.indexOf("://")
        if (scheme >= 0)
            value = value.substring(scheme + 3)
        var terminator = value.search(/[/?#]/)
        if (terminator >= 0)
            value = value.substring(0, terminator)
        if (value.charAt(0) === "[") {
            var bracket = value.indexOf("]")
            return bracket >= 0 && value.charAt(bracket + 1) === ":"
                ? value.substring(bracket + 2) : ""
        }
        var firstColon = value.indexOf(":")
        var lastColon = value.lastIndexOf(":")
        return firstColon > 0 && firstColon === lastColon
            ? value.substring(lastColon + 1) : ""
    }

    function isLoopbackHost(host) {
        var value = host.trim().toLowerCase()
        if (value.charAt(0) === "[" && value.charAt(value.length - 1) === "]")
            value = value.substring(1, value.length - 1)
        return value === "localhost" || value === "::1"
            || value.indexOf("127.") === 0
    }

    function validateConnectionDraft(scheme, host, port) {
        if (scheme !== "http" && scheme !== "https")
            return qsTr("Scheme must be HTTP or HTTPS")
        if (host.trim().length === 0)
            return qsTr("Host is required")
        if (/\s|[/@?#]/.test(host))
            return qsTr("Host must not contain spaces, credentials, paths, or query data")
        if (scheme === "http" && !isLoopbackHost(host))
            return qsTr("Remote daemons require HTTPS")
        if (!/^\d+$/.test(port.trim()))
            return qsTr("Port must be a number")
        var portValue = Number(port)
        if (portValue < 1 || portValue > 65535)
            return qsTr("Port must be between 1 and 65535")
        return ""
    }

    function applyConnectionDraft() {
        connectionValidationMessage = validateConnectionDraft(
            connectionSchemeDraft,
            connectionHostField.text,
            connectionPortField.text
        )
        if (connectionValidationMessage.length > 0)
            return

        var payload = {
            "host": connectionHostField.text.trim(),
            "port": Number(connectionPortField.text),
            "scheme": connectionSchemeDraft
        }
        var key = connectionApiKeyField.text
        if (clearApiKeyRequested) {
            payload.api_key_mode = "clear"
        } else if (key.length > 0) {
            payload.api_key_mode = "replace"
            payload.api_key = key
        } else {
            payload.api_key_mode = "keep"
        }
        dispatchRequested("connection_apply_requested", JSON.stringify(payload))
        connectionApiKeyField.text = ""
        clearApiKeyRequested = false
        key = ""
    }

    function parseCompactTimestamp(value) {
        var match = /^(\d{4})(\d{2})(\d{2})(\d{2})(\d{2})$/.exec(value)
        if (match === null)
            return NaN
        var year = Number(match[1])
        var month = Number(match[2])
        var day = Number(match[3])
        var hour = Number(match[4])
        var minute = Number(match[5])
        var parsed = new Date(year, month - 1, day, hour, minute)
        var instant = parsed.getTime()
        if (parsed.getFullYear() !== year
                || parsed.getMonth() !== month - 1
                || parsed.getDate() !== day
                || parsed.getHours() !== hour
                || parsed.getMinutes() !== minute)
            return NaN
        return instant
    }

    function parseRfc3339Timestamp(value) {
        var pattern = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})$/
        if (!pattern.test(value))
            return NaN
        var instant = Date.parse(value)
        return isNaN(instant) ? NaN : instant
    }

    function parseEventTimestamp(value) {
        var compact = parseCompactTimestamp(value)
        return isNaN(compact) ? parseRfc3339Timestamp(value) : compact
    }

    function validateEventDraft(id, from, to, content) {
        if (id.trim().length === 0)
            return qsTr("Event ID is required")
        if (content.trim().length === 0)
            return qsTr("Event content is required")
        var fromInstant = parseEventTimestamp(from.trim())
        var toInstant = parseEventTimestamp(to.trim())
        if (isNaN(fromInstant) || isNaN(toInstant))
            return qsTr("Use YYYYMMDDHHMM or RFC 3339 with a timezone offset")
        if (fromInstant >= toInstant)
            return qsTr("The start must be earlier than the end")
        return ""
    }

    function formatTimestamp(value) {
        if (!hasValue(value))
            return unavailableText
        var instant = Number(value)
        if (!isFinite(instant))
            return unavailableText
        var date = new Date(instant)
        return isNaN(date.getTime()) ? unavailableText : date.toLocaleString()
    }

    function receiptOutcomeColor(outcome) {
        switch (optionalText(outcome)) {
        case "succeeded":
            return IslandTheme.green
        case "failed":
            return IslandTheme.red
        case "cancelled":
            return IslandTheme.amber
        default:
            return IslandTheme.secondaryText
        }
    }

    function receiptsForMenu(receipts) {
        var supported = ["settings", "event", "maintenance", "autostart"]
        var result = []
        for (var index = 0; index < receipts.length; index += 1) {
            var receipt = objectOrEmpty(receipts[index])
            if (supported.indexOf(optionalText(receipt.operation)) >= 0)
                result.push(receipt)
        }
        return result
    }

    function failedReceipts(receipts) {
        return receipts.filter(function(receiptValue) {
            return objectOrEmpty(receiptValue).outcome === "failed"
        })
    }

    function connectionNeedsAttention() {
        return uiState.lifecycle === "offline" || uiState.lifecycle === "fatal"
            || hasValue(connection.error)
            || (connection.remote === true && connection.authenticated !== true)
    }

    function connectionAttentionText() {
        if (hasValue(connection.error))
            return qsTr("Daemon unavailable: %1").arg(connection.error)
        if (connection.remote === true && connection.authenticated !== true)
            return qsTr("Remote daemon authentication is required. Open Advanced to configure the API key.")
        return qsTr("The llmux daemon is offline. Existing display and startup preferences remain available.")
    }

    function surfaceModeText() {
        var mode = optionalText(capabilities.surface_mode)
        if (mode.length === 0)
            mode = optionalText(capabilities.presentation)
        if (mode.length === 0)
            mode = optionalText(windowState.presentation)
        return mode.length > 0 ? mode.replace(/_/g, " ") : unavailableText
    }

    function capabilityExplanation(name) {
        var capability = objectOrEmpty(capabilities[name])
        if (hasValue(capability.reason))
            return String(capability.reason)
        if (hasOwn(capability, "available"))
            return capability.available ? qsTr("Available") : qsTr("Unavailable on this session")
        return unavailableText
    }

    ColumnLayout {
        id: menuContent
        width: menuPage.availableWidth
        spacing: IslandTheme.sectionSpacing

        RowLayout {
            Layout.fillWidth: true

            ColumnLayout {
                spacing: 0
                Label {
                    text: qsTr("Settings")
                    color: IslandTheme.primaryText
                    font.pixelSize: 20
                    font.weight: Font.DemiBold
                }
                Label {
                    text: qsTr("Display, sound, privacy, and startup")
                    color: IslandTheme.secondaryText
                    font.pixelSize: 11
                }
            }

            Item { Layout.fillWidth: true }

            IslandButton {
                text: qsTr("Test notification")
                icon.name: "notifications"
                highlighted: true
                onClicked: menuPage.dispatchRequested("test_notification", "{}")
            }
            IslandButton {
                objectName: "menu-advanced-disclosure"
                text: qsTr("Advanced")
                checkable: true
                checked: menuPage.advancedVisible
                Accessible.name: qsTr("Show advanced settings")
                onClicked: menuPage.advancedVisible = checked
            }
        }

        IslandInlineMessage {
            objectName: "menu-connection-attention"
            Layout.fillWidth: true
            visible: menuPage.connectionNeedsAttention()
            type: menuPage.uiState.lifecycle === "fatal"
                || menuPage.hasValue(menuPage.connection.error)
                ? Kirigami.MessageType.Error : Kirigami.MessageType.Warning
            text: menuPage.connectionAttentionText()
        }

        IslandInlineMessage {
            Layout.fillWidth: true
            visible: menuPage.optionalText(menuPage.operation.id).length > 0
            type: Kirigami.MessageType.Information
            text: qsTr("Working on %1%2 · started %3")
                .arg(menuPage.displayText(menuPage.operation.kind))
                .arg(menuPage.hasValue(menuPage.operation.target_display)
                    ? qsTr(" for %1").arg(menuPage.operation.target_display) : "")
                .arg(menuPage.formatTimestamp(menuPage.operation.started_at_ms))
        }

        IslandCard {
            Layout.fillWidth: true

            contentItem: ColumnLayout {
                spacing: Kirigami.Units.smallSpacing

                IslandSectionLabel {
                    text: qsTr("Appearance and notifications")
                }

                GridLayout {
                    Layout.fillWidth: true
                    columns: 2
                    columnSpacing: 16
                    rowSpacing: 10

                    IslandFieldLabel { text: qsTr("Screen") }

                    IslandComboBox {
                        id: screenSelector
                        Layout.fillWidth: true
                        model: menuPage.screens
                        enabled: menuPage.screens.length > 0
                            && !menuPage.isOperationBusy("settings")
                        currentIndex: menuPage.selectedOptionIndex(
                            menuPage.screens,
                            menuPage.windowState.selected_screen_id
                        )
                        displayText: currentIndex >= 0
                            ? menuPage.optionLabel(menuPage.screens[currentIndex], currentIndex)
                            : menuPage.unavailableText
                        delegate: IslandItemDelegate {
                            required property var modelData
                            required property int index
                            width: screenSelector.width
                            text: menuPage.optionLabel(modelData, index)
                            highlighted: screenSelector.highlightedIndex === index
                        }
                        onActivated: function(index) {
                            var id = menuPage.optionId(menuPage.screens[index], index)
                            if (id.length > 0)
                                menuPage.dispatchRequested(
                                    "screen_selected",
                                    JSON.stringify({ "id": id })
                                )
                        }
                    }

                    IslandFieldLabel { text: qsTr("Sound") }
                    RowLayout {
                        Layout.fillWidth: true

                        IslandComboBox {
                            id: soundSelector
                            Layout.fillWidth: true
                            model: menuPage.sounds
                            enabled: menuPage.sounds.length > 0
                                && !menuPage.isOperationBusy("settings")
                            currentIndex: menuPage.selectedOptionIndex(
                                menuPage.sounds,
                                menuPage.settings.sound_id
                            )
                            displayText: currentIndex >= 0
                                ? menuPage.optionLabel(menuPage.sounds[currentIndex], currentIndex)
                                : menuPage.unavailableText
                            delegate: IslandItemDelegate {
                                required property var modelData
                                required property int index
                                width: soundSelector.width
                                text: menuPage.optionLabel(modelData, index)
                                highlighted: soundSelector.highlightedIndex === index
                            }
                            onActivated: function(index) {
                                var id = menuPage.optionId(menuPage.sounds[index], index)
                                if (id.length > 0)
                                    menuPage.dispatchRequested(
                                        "sound_selected",
                                        JSON.stringify({ "id": id })
                                    )
                            }
                        }

                        IslandButton {
                            text: qsTr("Preview")
                            icon.name: "media-playback-start"
                            enabled: soundSelector.currentIndex >= 0
                                && !menuPage.isOperationBusy("settings")
                            onClicked: {
                                var id = menuPage.optionId(
                                    menuPage.sounds[soundSelector.currentIndex],
                                    soundSelector.currentIndex
                                )
                                if (id.length > 0)
                                    menuPage.dispatchRequested(
                                        "sound_preview_requested",
                                        JSON.stringify({ "id": id })
                                    )
                            }
                        }
                    }

                    IslandFieldLabel { text: qsTr("Privacy") }
                    IslandSwitch {
                        Layout.fillWidth: true
                        text: qsTr("Anonymize account email")
                        checked: menuPage.settings.email_anonymous === true
                        enabled: !menuPage.isOperationBusy("settings")
                        onToggled: menuPage.dispatchRequested(
                            "email_anonymous_changed",
                            JSON.stringify({ "enabled": checked })
                        )
                    }

                    IslandFieldLabel {
                        visible: menuPage.advancedVisible
                        text: qsTr("Quota")
                    }
                    IslandSwitch {
                        Layout.fillWidth: true
                        visible: menuPage.advancedVisible
                        text: qsTr("Show Fable weekly quota")
                        checked: menuPage.settings.show_fable_weekly === true
                        enabled: !menuPage.isOperationBusy("settings")
                        onToggled: menuPage.dispatchRequested(
                            "show_fable_changed",
                            JSON.stringify({ "enabled": checked })
                        )
                    }
                }
            }
        }

        IslandCard {
            id: connectionCard
            objectName: "connection-settings"
            Layout.fillWidth: true
            visible: menuPage.advancedVisible

            contentItem: ColumnLayout {
                spacing: Kirigami.Units.smallSpacing

                RowLayout {
                    Layout.fillWidth: true
                    IslandSectionLabel {
                        text: qsTr("Daemon connection")
                    }
                    Item { Layout.fillWidth: true }
                    Label {
                        text: menuPage.displayText(menuPage.connection.endpoint_display)
                        elide: Text.ElideMiddle
                        color: IslandTheme.secondaryText
                    }
                }

                GridLayout {
                    Layout.fillWidth: true
                    columns: 2
                    columnSpacing: 16
                    rowSpacing: 10

                    IslandFieldLabel { text: qsTr("Scheme") }

                    IslandComboBox {
                        id: connectionSchemeSelector
                        objectName: "connection-scheme-selector"
                        Layout.fillWidth: true
                        model: ["http", "https"]
                        currentIndex: menuPage.connectionSchemeDraft === "https" ? 1 : 0
                        enabled: !menuPage.isOperationBusy("settings")
                        onActivated: function(index) {
                            menuPage.connectionSchemeDraft = model[index]
                            menuPage.connectionValidationMessage = ""
                        }
                    }

                    IslandFieldLabel { text: qsTr("Host") }
                    IslandTextField {
                        id: connectionHostField
                        Layout.fillWidth: true
                        text: menuPage.endpointHost(menuPage.connection.endpoint_display)
                        placeholderText: qsTr("127.0.0.1")
                        enabled: !menuPage.isOperationBusy("settings")
                        inputMethodHints: Qt.ImhNoPredictiveText
                        onTextEdited: menuPage.connectionValidationMessage = ""
                    }

                    IslandFieldLabel { text: qsTr("Port") }
                    IslandTextField {
                        id: connectionPortField
                        Layout.fillWidth: true
                        text: menuPage.endpointPort(menuPage.connection.endpoint_display)
                        placeholderText: qsTr("3456")
                        enabled: !menuPage.isOperationBusy("settings")
                        validator: IntValidator { bottom: 1; top: 65535 }
                        inputMethodHints: Qt.ImhDigitsOnly
                        onTextEdited: menuPage.connectionValidationMessage = ""
                    }

                    IslandFieldLabel { text: qsTr("API key") }
                    IslandTextField {
                        id: connectionApiKeyField
                        Layout.fillWidth: true
                        placeholderText: menuPage.settings.api_key_configured
                            ? qsTr("Configured · leave blank to keep")
                            : qsTr("Required for a remote daemon")
                        echoMode: TextInput.Password
                        passwordCharacter: "●"
                        enabled: !menuPage.isOperationBusy("settings")
                        inputMethodHints: Qt.ImhSensitiveData | Qt.ImhNoPredictiveText
                        onTextEdited: {
                            menuPage.connectionValidationMessage = ""
                            if (text.length > 0)
                                menuPage.clearApiKeyRequested = false
                        }
                    }

                    IslandFieldLabel {
                        visible: menuPage.settings.api_key_configured === true
                        text: qsTr("Stored key")
                    }
                    IslandCheckBox {
                        Layout.fillWidth: true
                        text: qsTr("Clear the stored API key")
                        visible: menuPage.settings.api_key_configured === true
                        checked: menuPage.clearApiKeyRequested
                        enabled: !menuPage.isOperationBusy("settings")
                        onToggled: {
                            menuPage.clearApiKeyRequested = checked
                            if (checked)
                                connectionApiKeyField.text = ""
                            menuPage.connectionValidationMessage = ""
                        }
                    }
                }

                IslandInlineMessage {
                    Layout.fillWidth: true
                    visible: menuPage.connectionValidationMessage.length > 0
                    type: Kirigami.MessageType.Error
                    text: menuPage.connectionValidationMessage
                }

                RowLayout {
                    Layout.fillWidth: true
                    Label {
                        text: menuPage.connection.remote === true
                            ? (menuPage.connection.authenticated === true
                                ? qsTr("Remote connection · authenticated")
                                : qsTr("Remote connection · API key required"))
                            : qsTr("Loopback connection")
                        color: IslandTheme.secondaryText
                    }
                    Item { Layout.fillWidth: true }
                    IslandButton {
                        text: qsTr("Apply connection")
                        icon.name: "network-connect"
                        enabled: !menuPage.isOperationBusy("settings")
                        onClicked: menuPage.applyConnectionDraft()
                    }
                }
            }
        }

        IslandCard {
            id: desktopCard
            objectName: "desktop-capabilities"
            Layout.fillWidth: true

            contentItem: ColumnLayout {
                spacing: Kirigami.Units.smallSpacing

                IslandSectionLabel {
                    text: qsTr("Startup")
                }

                IslandSwitch {
                    text: qsTr("Launch Islands at login")
                    checked: menuPage.autostart.enabled === true
                    enabled: menuPage.hasOwn(menuPage.autostart, "enabled")
                        && menuPage.autostart.available !== false
                        && !menuPage.isOperationBusy("autostart")
                    onToggled: menuPage.dispatchRequested(
                        "autostart_changed",
                        JSON.stringify({ "enabled": checked })
                    )
                }

                Label {
                    Layout.fillWidth: true
                    visible: !menuPage.hasOwn(menuPage.autostart, "enabled")
                    text: menuPage.unavailableText
                    color: IslandTheme.secondaryText
                }

                GridLayout {
                    objectName: "platform-diagnostics"
                    Layout.fillWidth: true
                    visible: menuPage.advancedVisible
                    columns: menuPage.width >= 700 ? 2 : 1
                    columnSpacing: Kirigami.Units.largeSpacing

                    ColumnLayout {
                        Layout.fillWidth: true
                        Label { text: qsTr("Surface mode"); font.bold: true }
                        Label {
                            Layout.fillWidth: true
                            text: menuPage.surfaceModeText()
                            wrapMode: Text.Wrap
                            color: IslandTheme.secondaryText
                        }
                    }

                    ColumnLayout {
                        Layout.fillWidth: true
                        Label { text: qsTr("Layer shell"); font.bold: true }
                        Label {
                            Layout.fillWidth: true
                            text: menuPage.capabilityExplanation("layer_shell")
                            wrapMode: Text.Wrap
                            color: IslandTheme.secondaryText
                        }
                    }

                    ColumnLayout {
                        Layout.fillWidth: true
                        Label { text: qsTr("System tray"); font.bold: true }
                        Label {
                            Layout.fillWidth: true
                            text: menuPage.capabilityExplanation("tray")
                            wrapMode: Text.Wrap
                            color: IslandTheme.secondaryText
                        }
                    }

                    ColumnLayout {
                        Layout.fillWidth: true
                        Label { text: qsTr("Notifications"); font.bold: true }
                        Label {
                            Layout.fillWidth: true
                            text: menuPage.capabilityExplanation("notifications")
                            wrapMode: Text.Wrap
                            color: IslandTheme.secondaryText
                        }
                    }

                    ColumnLayout {
                        objectName: "accessibility-capability"
                        Layout.fillWidth: true
                        Label { text: qsTr("Accessibility"); font.bold: true }
                        Label {
                            Layout.fillWidth: true
                            text: qsTr("Not required on Plasma; no global pointer monitoring")
                            wrapMode: Text.Wrap
                            color: IslandTheme.secondaryText
                        }
                    }
                }
            }
        }

        IslandCard {
            objectName: "events-settings"
            Layout.fillWidth: true
            visible: menuPage.advancedVisible

            contentItem: ColumnLayout {
                spacing: Kirigami.Units.smallSpacing

                RowLayout {
                    Layout.fillWidth: true
                    IslandSectionLabel {
                        text: qsTr("Events")
                    }
                    Item { Layout.fillWidth: true }
                    IslandButton {
                        text: qsTr("Add event")
                        icon.name: "list-add"
                        enabled: !menuPage.isOperationBusy("event")
                        onClicked: eventEditor.openForCreate()
                    }
                }

                Label {
                    Layout.fillWidth: true
                    visible: menuPage.events.length === 0
                    text: qsTr("No configured events")
                    color: IslandTheme.secondaryText
                }

                Repeater {
                    model: menuPage.events

                    delegate: IslandCard {
                        id: eventCard
                        required property var modelData
                        readonly property var event: menuPage.objectOrEmpty(modelData)
                        Layout.fillWidth: true

                        contentItem: RowLayout {
                            spacing: Kirigami.Units.largeSpacing

                            ColumnLayout {
                                Layout.fillWidth: true
                                Label {
                                    Layout.fillWidth: true
                                    text: menuPage.displayText(eventCard.event.id)
                                    font.bold: true
                                    elide: Text.ElideRight
                                }
                                Label {
                                    Layout.fillWidth: true
                                    text: qsTr("%1 → %2")
                                        .arg(menuPage.displayText(eventCard.event.from))
                                        .arg(menuPage.displayText(eventCard.event.to))
                                    color: IslandTheme.secondaryText
                                    elide: Text.ElideRight
                                }
                                Label {
                                    Layout.fillWidth: true
                                    text: menuPage.displayText(eventCard.event.content)
                                    wrapMode: Text.Wrap
                                }
                            }

                            IslandButton {
                                text: qsTr("Edit")
                                icon.name: "document-edit"
                                enabled: !menuPage.isOperationBusy("event")
                                onClicked: eventEditor.openForEdit(eventCard.event)
                            }

                            IslandButton {
                                text: qsTr("Remove")
                                icon.name: "edit-delete"
                                enabled: menuPage.hasValue(eventCard.event.id)
                                    && !menuPage.isOperationBusy("event")
                                onClicked: {
                                    menuPage.pendingEvent = eventCard.event
                                    eventRemoveConfirmation.open()
                                }
                            }
                        }
                    }
                }
            }
        }

        IslandCard {
            id: maintenanceCard
            objectName: "maintenance-settings"
            Layout.fillWidth: true
            visible: menuPage.advancedVisible

            contentItem: ColumnLayout {
                spacing: Kirigami.Units.smallSpacing

                IslandSectionLabel {
                    text: qsTr("Updates")
                }

                GridLayout {
                    Layout.fillWidth: true
                    columns: 2
                    columnSpacing: 16
                    rowSpacing: 8

                    IslandFieldLabel { text: qsTr("Installed version") }
                    Label {
                        text: menuPage.displayText(menuPage.maintenance.version)
                        color: IslandTheme.primaryText
                    }
                    IslandFieldLabel { text: qsTr("Latest version") }
                    Label {
                        text: menuPage.displayText(menuPage.maintenance.latest_version)
                        color: IslandTheme.primaryText
                    }
                    IslandFieldLabel { text: qsTr("Install owner") }
                    Label {
                        text: menuPage.displayText(menuPage.maintenance.install_owner)
                        color: IslandTheme.primaryText
                    }
                    IslandFieldLabel { text: qsTr("Update available") }
                    Label {
                        text: menuPage.hasOwn(menuPage.maintenance, "update_available")
                            ? (menuPage.maintenance.update_available ? qsTr("Yes") : qsTr("No"))
                            : menuPage.unavailableText
                        color: menuPage.maintenance.update_available === true
                            ? IslandTheme.amber : IslandTheme.primaryText
                    }

                    IslandFieldLabel { text: qsTr("Release channel") }
                    IslandSegmentedControl {
                        id: channelSelector
                        Layout.fillWidth: true
                        model: ["stable", "preview"]
                        currentIndex: menuPage.currentChannel === "stable" ? 0
                            : menuPage.currentChannel === "preview" ? 1 : -1
                        enabled: !menuPage.isOperationBusy("maintenance")
                        onActivated: function(index) {
                            var selected = model[index]
                            if (selected !== menuPage.currentChannel) {
                                menuPage.pendingChannel = selected
                                channelChangeConfirmation.open()
                            }
                        }
                    }
                }

                IslandInlineMessage {
                    Layout.fillWidth: true
                    visible: menuPage.hasValue(menuPage.maintenance.instructions)
                    type: Kirigami.MessageType.Information
                    text: menuPage.optionalText(menuPage.maintenance.instructions)
                }

                RowLayout {
                    Layout.fillWidth: true
                    Label {
                        Layout.fillWidth: true
                        text: qsTr("Package ownership is checked before any update is attempted.")
                        wrapMode: Text.Wrap
                        color: IslandTheme.secondaryText
                    }
                    IslandButton {
                        text: menuPage.maintenance.update_available === true
                            ? qsTr("Update now") : qsTr("Check for updates")
                        icon.name: "system-software-update"
                        enabled: !menuPage.isOperationBusy("maintenance")
                        onClicked: menuPage.dispatchRequested("update_requested", "{}")
                    }
                }
            }
        }

        IslandCard {
            objectName: "menu-verification-receipts"
            Layout.fillWidth: true
            visible: menuPage.advancedVisible
                || menuPage.visibleMenuReceiptItems.length > 0

            contentItem: ColumnLayout {
                spacing: Kirigami.Units.smallSpacing

                IslandSectionLabel {
                    text: qsTr("Verification receipts")
                }

                Label {
                    Layout.fillWidth: true
                    visible: menuPage.visibleMenuReceiptItems.length === 0
                    text: qsTr("No completed settings, event, maintenance, or autostart operations")
                    color: IslandTheme.secondaryText
                    wrapMode: Text.Wrap
                }

                Repeater {
                    model: menuPage.visibleMenuReceiptItems

                    delegate: RowLayout {
                        id: receiptRow
                        required property var modelData
                        readonly property var receipt: menuPage.objectOrEmpty(modelData)
                        Layout.fillWidth: true
                        spacing: Kirigami.Units.largeSpacing

                        Rectangle {
                            radius: width / 2
                            implicitWidth: Kirigami.Units.smallSpacing
                            implicitHeight: implicitWidth
                            color: menuPage.receiptOutcomeColor(receiptRow.receipt.outcome)
                        }

                        ColumnLayout {
                            Layout.fillWidth: true
                            spacing: 0
                            Label {
                                Layout.fillWidth: true
                                text: qsTr("%1 · %2%3")
                                    .arg(menuPage.displayText(receiptRow.receipt.operation))
                                    .arg(menuPage.displayText(receiptRow.receipt.outcome))
                                    .arg(menuPage.hasValue(receiptRow.receipt.target_display)
                                        ? qsTr(" · %1").arg(receiptRow.receipt.target_display) : "")
                                font.bold: true
                                font.pixelSize: 10
                                elide: Text.ElideRight
                            }
                            Label {
                                Layout.fillWidth: true
                                text: menuPage.displayText(receiptRow.receipt.message)
                                wrapMode: Text.Wrap
                                color: IslandTheme.secondaryText
                                font.pixelSize: 10
                            }
                            Label {
                                Layout.fillWidth: true
                                text: qsTr("Started %1 · finished %2")
                                    .arg(menuPage.formatTimestamp(receiptRow.receipt.started_at_ms))
                                    .arg(menuPage.formatTimestamp(receiptRow.receipt.finished_at_ms))
                                color: IslandTheme.secondaryText
                                font.family: IslandTheme.monoFamily
                                font.pixelSize: 9
                                elide: Text.ElideRight
                            }
                        }
                    }
                }
            }
        }

        IslandCard {
            objectName: "about-llmux-islands"
            Layout.fillWidth: true
            visible: menuPage.advancedVisible

            contentItem: ColumnLayout {
                spacing: Kirigami.Units.smallSpacing

                IslandSectionLabel {
                    text: qsTr("About llmux Islands")
                }

                GridLayout {
                    Layout.fillWidth: true
                    columns: 2
                    columnSpacing: 16
                    rowSpacing: 8

                    IslandFieldLabel { text: qsTr("Islands version") }
                    Label {
                        objectName: "about-islands-version"
                        text: menuPage.displayText(menuPage.aboutIslandsVersion)
                        color: IslandTheme.primaryText
                    }
                    IslandFieldLabel { text: qsTr("Daemon version") }
                    Label {
                        objectName: "about-daemon-version"
                        text: menuPage.displayText(menuPage.aboutDaemonVersion)
                        color: IslandTheme.primaryText
                    }
                    IslandFieldLabel { text: qsTr("License") }
                    Label {
                        objectName: "about-license"
                        text: menuPage.displayText(menuPage.aboutLicense)
                        color: IslandTheme.primaryText
                    }
                    IslandFieldLabel { text: qsTr("Source") }
                    Label {
                        objectName: "about-source-url"
                        text: menuPage.displayText(menuPage.aboutSourceUrl)
                        color: IslandTheme.secondaryText
                        font.family: IslandTheme.monoFamily
                        elide: Text.ElideMiddle
                    }
                }

                RowLayout {
                    Layout.fillWidth: true
                    IslandButton {
                        text: qsTr("Open source")
                        icon.name: "internet-services"
                        enabled: menuPage.hasValue(menuPage.aboutSourceUrl)
                        onClicked: menuPage.dispatchRequested(
                            "open_url_requested",
                            JSON.stringify({ "url": menuPage.aboutSourceUrl })
                        )
                    }
                    IslandButton {
                        objectName: "open-releases"
                        text: qsTr("Releases")
                        icon.name: "system-software-update"
                        enabled: menuPage.aboutReleasesUrl.length > 0
                        onClicked: menuPage.dispatchRequested(
                            "open_url_requested",
                            JSON.stringify({ "url": menuPage.aboutReleasesUrl })
                        )
                    }
                    Item { Layout.fillWidth: true }
                    IslandButton {
                        text: qsTr("Quit Islands")
                        icon.name: "application-exit"
                        onClicked: menuPage.dispatchRequested("quit_requested", "{}")
                    }
                }
            }
        }
    }

    IslandDialog {
        id: eventEditor
        objectName: "event-editor"
        parent: Overlay.overlay
        anchors.centerIn: parent
        width: Math.min(560, parent ? parent.width - Kirigami.Units.gridUnit * 2 : 560)
        modal: true
        focus: true
        closePolicy: Popup.CloseOnEscape
        title: editingExisting ? qsTr("Edit event") : qsTr("Create event")

        property bool editingExisting: false
        property string originalId: ""
        property string validationMessage: ""

        function openForCreate() {
            editingExisting = false
            originalId = ""
            eventIdField.text = ""
            eventFromField.text = ""
            eventToField.text = ""
            eventContentField.text = ""
            validationMessage = ""
            open()
            eventIdField.forceActiveFocus()
        }

        function openForEdit(event) {
            var draft = menuPage.objectOrEmpty(event)
            editingExisting = true
            originalId = menuPage.optionalText(draft.id)
            eventIdField.text = menuPage.optionalText(draft.id)
            eventFromField.text = menuPage.optionalText(draft.from)
            eventToField.text = menuPage.optionalText(draft.to)
            eventContentField.text = menuPage.optionalText(draft.content)
            validationMessage = ""
            open()
            eventFromField.forceActiveFocus()
        }

        function submit() {
            validationMessage = menuPage.validateEventDraft(
                eventIdField.text,
                eventFromField.text,
                eventToField.text,
                eventContentField.text
            )
            if (validationMessage.length > 0)
                return
            menuPage.dispatchRequested(
                "event_upsert_requested",
                JSON.stringify({
                    "event": {
                        "id": eventIdField.text.trim(),
                        "from": eventFromField.text.trim(),
                        "to": eventToField.text.trim(),
                        "content": eventContentField.text.trim()
                    }
                })
            )
            close()
        }

        contentItem: ColumnLayout {
            spacing: Kirigami.Units.smallSpacing

            IslandTextField {
                id: eventIdField
                Layout.fillWidth: true
                placeholderText: qsTr("Event ID")
                readOnly: eventEditor.editingExisting && eventEditor.originalId.length > 0
                onTextEdited: eventEditor.validationMessage = ""
            }
            IslandTextField {
                id: eventFromField
                Layout.fillWidth: true
                placeholderText: qsTr("From · YYYYMMDDHHMM or RFC 3339")
                inputMethodHints: Qt.ImhNoPredictiveText
                onTextEdited: eventEditor.validationMessage = ""
            }
            IslandTextField {
                id: eventToField
                Layout.fillWidth: true
                placeholderText: qsTr("To · YYYYMMDDHHMM or RFC 3339")
                inputMethodHints: Qt.ImhNoPredictiveText
                onTextEdited: eventEditor.validationMessage = ""
            }
            IslandTextArea {
                id: eventContentField
                Layout.fillWidth: true
                Layout.preferredHeight: Kirigami.Units.gridUnit * 5
                placeholderText: qsTr("Event content")
                wrapMode: TextEdit.Wrap
                onTextChanged: eventEditor.validationMessage = ""
            }
            IslandInlineMessage {
                Layout.fillWidth: true
                visible: eventEditor.validationMessage.length > 0
                type: Kirigami.MessageType.Error
                text: eventEditor.validationMessage
            }
            RowLayout {
                Layout.fillWidth: true
                Item { Layout.fillWidth: true }
                IslandButton {
                    text: qsTr("Cancel")
                    onClicked: eventEditor.close()
                }
                IslandButton {
                    text: eventEditor.editingExisting ? qsTr("Save") : qsTr("Create")
                    highlighted: true
                    onClicked: eventEditor.submit()
                }
            }
        }
    }

    IslandDialog {
        id: eventRemoveConfirmation
        objectName: "event-remove-confirmation"
        parent: Overlay.overlay
        anchors.centerIn: parent
        modal: true
        focus: true
        width: Math.min(480, parent ? parent.width - Kirigami.Units.gridUnit * 2 : 480)
        title: qsTr("Remove event?")
        standardButtons: Dialog.Yes | Dialog.Cancel
        contentItem: Label {
            text: qsTr("Remove %1? This changes the daemon configuration.")
                .arg(menuPage.displayText(menuPage.pendingEvent.id))
            wrapMode: Text.Wrap
        }
        onAccepted: {
            var id = menuPage.optionalText(menuPage.pendingEvent.id)
            if (id.length > 0)
                menuPage.dispatchRequested(
                    "event_remove_requested",
                    JSON.stringify({ "id": id })
                )
            menuPage.pendingEvent = ({})
        }
        onRejected: menuPage.pendingEvent = ({})
    }

    IslandDialog {
        id: channelChangeConfirmation
        objectName: "channel-change-confirmation"
        parent: Overlay.overlay
        anchors.centerIn: parent
        modal: true
        focus: true
        width: Math.min(520, parent ? parent.width - Kirigami.Units.gridUnit * 2 : 520)
        title: qsTr("Change release channel?")
        standardButtons: Dialog.Yes | Dialog.Cancel
        contentItem: Label {
            text: qsTr("Change from %1 to %2 for both daemon and Islands package policy?")
                .arg(menuPage.displayText(menuPage.currentChannel))
                .arg(menuPage.displayText(menuPage.pendingChannel))
            wrapMode: Text.Wrap
        }
        onAccepted: {
            if (menuPage.pendingChannel === "stable"
                    || menuPage.pendingChannel === "preview")
                menuPage.dispatchRequested(
                    "channel_change_requested",
                    JSON.stringify({ "channel": menuPage.pendingChannel })
                )
            menuPage.pendingChannel = ""
        }
        onRejected: menuPage.pendingChannel = ""
    }
}
