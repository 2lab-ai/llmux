pragma ComponentBehavior: Bound

import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import Qt.labs.platform as Platform
import org.kde.kirigami as Kirigami
import io.twolab.LlmuxIslands 1.0

Kirigami.ApplicationWindow {
    id: root

    readonly property var uiState: parseState(controller.stateJson)
    readonly property var connection: uiState.connection || ({})
    readonly property var windowState: uiState.window || ({})
    readonly property string selectedSurface: uiState.navigation || "usage"
    readonly property int totalInFlight: inFlightTotal(windowState.provider_in_flight)
    readonly property bool trayNeedsAttention: healthNeedsAttention()
    readonly property bool trayAvailable: trayLoader.item !== null
        && trayLoader.item.available === true
    readonly property bool semanticOpen: windowState.open === true
    readonly property int preferredWindowWidth: semanticOpen
        ? (controller.snapshotMode ? 960 : expandedPreferredWidth()) : 260
    readonly property int preferredWindowContentHeight: semanticOpen
        ? (controller.snapshotMode ? snapshotPreferredContentHeight()
                                   : expandedPreferredContentHeight()) : 44
    readonly property var snapshotSurfaces: ["usage", "usage-advanced", "statistics", "statistics-advanced", "receipts", "menu", "menu-advanced"]
    property bool surfaceConfigured: false
    property bool noTrayFallback: false
    property bool bootPresentationStarted: false
    property bool openHadActiveFocus: false
    property bool quitRequested: false
    property bool snapshotCaptureBusy: false
    property int snapshotCaptureAttempts: 0
    property int snapshotIndex: -1
    property string dispatchError: ""

    function emptyState() {
        return {
            "schema_version": 1,
            "revision": 0,
            "lifecycle": "starting",
            "window": {
                "open": false,
                "provider_in_flight": {}
            },
            "navigation": "usage",
            "connection": {
                "endpoint_display": "http://127.0.0.1:3456",
                "error": null
            },
            "usage": {
                "accounts": [],
                "current_by_group": {},
                "provider_in_flight": {},
                "login": { "phase": "idle" }
            },
            "statistics": {},
            "settings": {},
            "operation": null,
            "notices": [],
            "verification_receipts": []
        }
    }

    function parseState(raw) {
        try {
            var parsed = JSON.parse(raw)
            return parsed !== null && typeof parsed === "object"
                ? parsed : emptyState()
        } catch (error) {
            return emptyState()
        }
    }

    function routeDispatch(action, payload) {
        var payloadJson = typeof payload === "string"
            ? payload : JSON.stringify(payload || {})
        controller.dispatch(action, payloadJson)
    }

    function publishDesktopCapabilities() {
        var trayAvailable = root.trayAvailable
        routeDispatch("desktop_capabilities_changed", {
            "tray_available": trayAvailable,
            // This shell delivers native notification UI through the Qt tray
            // adapter. Sound preview remains a separate best-effort adapter.
            "notifications_available": trayAvailable
        })
        synchronizeTrayFallback(trayAvailable)
    }

    function synchronizeTrayFallback(trayAvailable) {
        if (!surfaceConfigured || controller.smokeMode || controller.snapshotMode)
            return

        var fallbackRequired = !trayAvailable
        if (noTrayFallback === fallbackRequired)
            return

        noTrayFallback = fallbackRequired
        if (fallbackRequired && !semanticOpen)
            requestOpen("boot")
    }

    function selectSurface(surface) {
        routeDispatch("navigation_selected", { "navigation": surface })
    }

    function requestOpen(reason) {
        routeDispatch("open_requested", { "reason": reason })
    }

    function requestClose() {
        routeDispatch("close_requested", {})
    }

    function finiteMetric(value, fallback) {
        var number = Number(value)
        return isFinite(number) && number > 0 ? Math.round(number) : fallback
    }

    function selectedScreenDimension(name, fallback) {
        if (root.screen === null || root.screen === undefined)
            return fallback
        return finiteMetric(root.screen[name], fallback)
    }

    function expandedPreferredWidth() {
        var available = selectedScreenDimension("width", 960) - 32
        return Math.max(420, Math.min(960, available))
    }

    function expandedPreferredContentHeight() {
        var pageHeight = surfaceLoader.item === null
            || surfaceLoader.item === undefined ? 0
            : Number(surfaceLoader.item.preferredContentHeight)
        if (!isFinite(pageHeight) || pageHeight <= 0) {
            pageHeight = selectedSurface === "menu" ? 680
                : selectedSurface === "statistics" ? 620 : 560
        }
        var headerHeight = finiteMetric(expandedHeader.implicitHeight, 52)
        var available = selectedScreenDimension("height", 720) - 32
        return Math.max(360, Math.min(available, pageHeight + headerHeight + 32))
    }

    function snapshotPreferredContentHeight() {
        var pageHeight = surfaceLoader.item === null
            || surfaceLoader.item === undefined ? 0
            : Number(surfaceLoader.item.preferredContentHeight)
        if (!isFinite(pageHeight) || pageHeight <= 0)
            return 760
        var headerHeight = finiteMetric(expandedHeader.implicitHeight, 52)
        return Math.max(760, Math.min(2160, Math.ceil(pageHeight) + headerHeight + 32))
    }

    function dispatchPreferredWindowMetrics() {
        if (!surfaceConfigured)
            return
        var width = preferredWindowWidth
        var contentHeight = preferredWindowContentHeight
        if (finiteMetric(windowState.width, 0) === width
                && finiteMetric(windowState.content_height, 0) === contentHeight)
            return
        routeDispatch("window_metrics_changed", {
            "width": width,
            "content_height": contentHeight
        })
    }

    function screenId(screen, index) {
        var identity = [
            screen.name || "",
            screen.manufacturer || "",
            screen.model || "",
            screen.serialNumber || ""
        ].join("|")
        if (identity.replace(/\|/g, "").length === 0)
            identity = "index-" + index
        var hash = 2166136261
        for (var offset = 0; offset < identity.length; offset += 1) {
            hash ^= identity.charCodeAt(offset)
            hash = ((hash << 5) - hash) >>> 0
        }
        return "qt-screen:" + hash.toString(16)
    }

    function screenLabel(screen, index) {
        var name = screen.name || screen.model || qsTr("Screen %1").arg(index + 1)
        var width = Number(screen.width)
        var height = Number(screen.height)
        if (isFinite(width) && isFinite(height) && width > 0 && height > 0)
            return qsTr("%1 · %2×%3").arg(name).arg(width).arg(height)
        return name
    }

    function publishScreenInventory() {
        var inventory = []
        for (var index = 0; index < Application.screens.length; index += 1) {
            var screen = Application.screens[index]
            inventory.push({
                "id": screenId(screen, index),
                "label": screenLabel(screen, index)
            })
        }
        routeDispatch("screen_inventory_changed", { "screens": inventory })
    }

    function applySelectedScreen() {
        if (Application.screens.length === 0)
            return
        var semanticWindow = root.windowState || ({})
        var selected = semanticWindow.selected_screen_id || "auto"
        var target = Application.screens[0]
        if (selected !== "auto") {
            for (var index = 0; index < Application.screens.length; index += 1) {
                if (screenId(Application.screens[index], index) === selected) {
                    target = Application.screens[index]
                    break
                }
            }
        }
        root.screen = target
        if (controller.surfaceMode === "x11-positioned") {
            root.x = Number(target.virtualX) + (Number(target.width) - root.width) / 2
            root.y = Number(target.virtualY) + 8
        }
    }

    function inFlightTotal(counts) {
        var total = 0
        if (counts === null || typeof counts !== "object")
            return total
        for (var provider in counts) {
            if (!Object.prototype.hasOwnProperty.call(counts, provider))
                continue
            var count = Number(counts[provider])
            if (isFinite(count) && count > 0)
                total += count
        }
        return total
    }

    function connectionLabel() {
        if (uiState.lifecycle === "ready")
            return qsTr("Connected")
        if (uiState.lifecycle === "starting")
            return qsTr("Connecting")
        return qsTr("Offline")
    }

    function providerDisplayName(provider) {
        switch (String(provider).toLowerCase()) {
        case "claude": return qsTr("Claude")
        case "codex": return qsTr("Codex")
        case "grok": return qsTr("Grok")
        case "api": return qsTr("API")
        default: return String(provider)
        }
    }

    function providerInFlightSummary(includeIdle) {
        var counts = windowState.provider_in_flight
        var providers = []
        if (counts !== null && typeof counts === "object") {
            var keys = Object.keys(counts).sort()
            for (var index = 0; index < keys.length; index += 1) {
                var count = Number(counts[keys[index]])
                if (isFinite(count) && count >= 0 && (includeIdle || count > 0)) {
                    providers.push(qsTr("%1 %2")
                        .arg(providerDisplayName(keys[index]))
                        .arg(Math.floor(count)))
                }
            }
        }
        if (providers.length > 0)
            return providers.join(" · ")
        return includeIdle ? qsTr("No provider grouping available") : qsTr("Idle")
    }

    function healthNeedsAttention() {
        if (uiState.lifecycle !== "ready" || connection.error)
            return true
        var usage = uiState.usage || ({})
        var accounts = Array.isArray(usage.accounts) ? usage.accounts : []
        for (var index = 0; index < accounts.length; index += 1) {
            var account = accounts[index] || ({})
            if (account.healthy === false
                    || account.warning_level === "warning"
                    || account.warning_level === "critical")
                return true
        }
        var notices = Array.isArray(uiState.notices) ? uiState.notices : []
        for (var noticeIndex = 0; noticeIndex < notices.length; noticeIndex += 1) {
            var level = String((notices[noticeIndex] || ({})).level || "")
            if (level === "warning" || level === "error")
                return true
        }
        return false
    }

    function trayStatusText() {
        if (uiState.lifecycle !== "ready")
            return qsTr("Connection attention · %1").arg(connectionLabel())
        if (connection.error)
            return qsTr("Connection attention · %1").arg(connection.error)
        if (trayNeedsAttention)
            return qsTr("Account health needs attention")
        return qsTr("Connected and healthy")
    }

    function trayTooltip() {
        return qsTr("llmux Islands\nProviders: %1\nTotal: %2 in flight\nStatus: %3")
            .arg(providerInFlightSummary(true))
            .arg(totalInFlight)
            .arg(trayStatusText())
    }

    function compactProviderSummary() {
        return providerInFlightSummary(false)
    }

    function beginStartupPresentation() {
        if (bootPresentationStarted || controller.smokeMode || controller.snapshotMode)
            return
        bootPresentationStarted = true
        requestOpen("boot")
        trayFallbackTimer.restart()
        startupBootCloseTimer.restart()
    }

    function beginSnapshotRun() {
        if (!controller.snapshotMode || snapshotIndex >= 0)
            return
        snapshotIndex = 0
        requestOpen("boot")
        selectSnapshotSurface(snapshotSurfaces[snapshotIndex])
        snapshotCaptureTimer.restart()
    }

    function snapshotRoute(name) {
        if (name === "usage-advanced")
            return "usage"
        if (name === "statistics-advanced" || name === "receipts")
            return "statistics"
        if (name === "menu-advanced")
            return "menu"
        return name
    }

    function selectSnapshotSurface(name) {
        selectSurface(snapshotRoute(name))
    }

    function failSnapshot(message) {
        console.error("Snapshot failure: " + message)
        dispatchError = message
        quitRequested = true
        controller.failHeadless(message)
    }

    function retrySnapshot(message) {
        snapshotCaptureAttempts += 1
        if (snapshotCaptureAttempts >= 50) {
            failSnapshot(message)
            return
        }
        snapshotCaptureTimer.restart()
    }

    function snapshotSurfaceCount(name) {
        var item = surfaceLoader.item
        if (item === null || item === undefined || typeof item[name] !== "function")
            return -1
        var count = Number(item[name]())
        return isFinite(count) ? count : -1
    }

    function snapshotSurfaceFlag(name) {
        var item = surfaceLoader.item
        return item !== null && item !== undefined && item[name] === true
    }

    function captureSnapshotSurface() {
        if (!controller.snapshotMode || snapshotCaptureBusy || snapshotIndex < 0)
            return
        if (!semanticOpen || surfaceLoader.status !== Loader.Ready
                || width !== 960 || height !== preferredWindowContentHeight) {
            retrySnapshot(qsTr("Snapshot surface did not become ready"))
            return
        }

        snapshotCaptureBusy = true
        var surface = snapshotSurfaces[snapshotIndex]
        if ((surface === "usage-advanced" || surface === "statistics-advanced"
                || surface === "menu-advanced")
                && !snapshotSurfaceFlag("advancedVisible")) {
            snapshotCaptureBusy = false
            retrySnapshot(qsTr("Advanced snapshot surface did not become ready"))
            return
        }
        if ((surface === "usage" || surface === "usage-advanced")
                && snapshotSurfaceCount("renderedGaugeCount") < 1) {
            snapshotCaptureBusy = false
            retrySnapshot(qsTr("Usage snapshot rendered no quota gauges"))
            return
        }
        if (surface === "statistics-advanced"
                && (snapshotSurfaceCount("renderedHeatmapCellCount") < 1
                    || snapshotSurfaceCount("renderedServingAccountCount") < 1)) {
            snapshotCaptureBusy = false
            retrySnapshot(qsTr("Statistics snapshot rendered incomplete nested data"))
            return
        }
        if (surface === "receipts"
                && snapshotSurfaceCount("renderedVerificationReceiptCount") < 1) {
            snapshotCaptureBusy = false
            retrySnapshot(qsTr("Receipt snapshot rendered no verification receipt"))
            return
        }
        var outputPath = controller.snapshotDir + "/" + surface + ".png"
        var saveResult = function(result) {
            var saved = result !== null && result.saveToFile(outputPath)
            snapshotCaptureBusy = false
            if (!saved) {
                root.failSnapshot(qsTr("Failed to save %1 snapshot").arg(surface))
                return
            }

            snapshotIndex += 1
            snapshotCaptureAttempts = 0
            if (snapshotIndex >= snapshotSurfaces.length) {
                root.quitRequested = true
                controller.exitHeadless(0)
                return
            }
            selectSnapshotSurface(snapshotSurfaces[snapshotIndex])
            snapshotCaptureTimer.restart()
        }
        snapshotTarget.grabToImage(saveResult, Qt.size(960, height))
    }

    function automaticPollAllowed() {
        var retryAt = Number(connection.retry_at_ms)
        return !isFinite(retryAt) || retryAt <= 0 || Date.now() >= retryAt
    }

    function showNativeTrayMessage(payload) {
        var message
        try {
            message = JSON.parse(payload)
        } catch (error) {
            dispatchError = qsTr("Invalid notification payload")
            return
        }
        var tray = trayLoader.item
        if (tray !== null && tray.available) {
            tray.showMessage(
                String(message.title || qsTr("llmux Islands")),
                String(message.body || ""),
                Platform.SystemTrayIcon.Information,
                10000
            )
        }
    }

    width: finiteMetric(windowState.width, 260)
    height: finiteMetric(windowState.content_height, 44)
    minimumWidth: 1
    minimumHeight: 1
    visible: surfaceConfigured && (semanticOpen
        || controller.surfaceMode === "wayland-layer-shell" || noTrayFallback)
    title: qsTr("llmux Islands")
    color: semanticOpen ? IslandTheme.panel : "transparent"
    palette.window: IslandTheme.panel
    palette.windowText: IslandTheme.primaryText
    palette.base: IslandTheme.field
    palette.alternateBase: IslandTheme.surface
    palette.text: IslandTheme.primaryText
    palette.button: IslandTheme.surface
    palette.buttonText: IslandTheme.primaryText
    palette.highlight: IslandTheme.primaryText
    palette.highlightedText: IslandTheme.panel
    palette.placeholderText: IslandTheme.tertiaryText

    IslandsController {
        id: controller
    }

    TextEdit {
        id: clipboardHelper
        visible: false
    }

    Connections {
        target: controller

        function onPlatformCommand(command, payload) {
            if (command === "open_url") {
                Qt.openUrlExternally(payload)
            } else if (command === "copy_text") {
                clipboardHelper.text = payload
                clipboardHelper.selectAll()
                clipboardHelper.copy()
                clipboardHelper.deselect()
            } else if (command === "quit") {
                root.quitRequested = true
                Qt.quit()
            } else if (command === "show_notification") {
                root.showNativeTrayMessage(payload)
            } else if (command === "dispatch_error") {
                root.dispatchError = payload
            }
        }
    }

    // Headless smoke tests and deterministic snapshot runs must never create a
    // native tray backend. Qt's tray object owns platform resources even while
    // invisible, and destroying those resources during a headless shutdown is
    // not reliable across Qt/platform-plugin combinations.
    Loader {
        id: trayLoader
        active: !controller.smokeMode && !controller.snapshotMode
        sourceComponent: trayComponent
    }

    Component {
        id: trayComponent

        Platform.SystemTrayIcon {
            visible: available
            tooltip: root.trayTooltip()
            // Qt's portable tray API does not expose StatusNotifierItem status.
            // A theme warning icon plus explicit tooltip/menu text is the honest
            // cross-desktop attention representation available here.
            icon.name: root.trayNeedsAttention ? "dialog-warning" : "io.twolab.LlmuxIslands"
            icon.source: root.trayNeedsAttention
                ? "" : "qrc:/icons/io.twolab.LlmuxIslands.svg"

            onActivated: function(reason) {
                if (reason === Platform.SystemTrayIcon.Trigger
                        || reason === Platform.SystemTrayIcon.DoubleClick
                        || reason === Platform.SystemTrayIcon.MiddleClick) {
                    root.routeDispatch("tray_activated", {})
                }
            }

            onMessageClicked: root.requestOpen("notification")

            menu: Platform.Menu {
                Platform.MenuItem {
                    text: root.trayStatusText()
                    enabled: false
                }
                Platform.MenuItem {
                    text: qsTr("Providers: %1 · Total: %2")
                        .arg(root.providerInFlightSummary(true))
                        .arg(root.totalInFlight)
                    enabled: false
                }
                Platform.MenuSeparator {}
                Platform.MenuItem {
                    text: root.semanticOpen ? qsTr("Close") : qsTr("Open")
                    onTriggered: root.routeDispatch("tray_activated", {})
                }
                Platform.MenuItem {
                    text: qsTr("Refresh")
                    onTriggered: root.routeDispatch("refresh_requested", { "source": "manual" })
                }
                Platform.MenuSeparator {}
                Platform.MenuItem {
                    text: qsTr("Quit")
                    onTriggered: root.routeDispatch("quit_requested", {})
                }
            }
        }
    }

    Item {
        id: snapshotTarget
        anchors.fill: parent

        ToolBar {
            id: expandedHeader
            anchors.top: parent.top
            anchors.left: parent.left
            anchors.right: parent.right
            visible: root.semanticOpen
            height: visible ? implicitHeight : 0
            implicitHeight: 58

            background: Rectangle {
                color: IslandTheme.panel
                border.color: IslandTheme.border
                border.width: 0

                Rectangle {
                    anchors.left: parent.left
                    anchors.right: parent.right
                    anchors.bottom: parent.bottom
                    height: 1
                    color: IslandTheme.border
                }
            }

            contentItem: RowLayout {
                spacing: 14

                Label {
                    text: qsTr("llmux Islands")
                    color: IslandTheme.primaryText
                    font.pixelSize: 15
                    font.weight: Font.DemiBold
                    Layout.leftMargin: IslandTheme.pagePadding
                }

                Item {
                    Layout.fillWidth: true
                }

                IslandSegmentedControl {
                    Layout.preferredWidth: 300
                    model: [qsTr("Usage"), qsTr("Statistics"), qsTr("Settings")]
                    currentIndex: root.selectedSurface === "statistics" ? 1
                        : root.selectedSurface === "menu" ? 2 : 0
                    onActivated: function(index) {
                        root.selectSurface(index === 1 ? "statistics" : index === 2 ? "menu" : "usage")
                    }
                }

                Rectangle {
                    radius: 0
                    implicitWidth: connectionStatus.implicitWidth
                        + 22
                    implicitHeight: connectionStatus.implicitHeight
                        + 12
                    color: root.uiState.lifecycle === "ready"
                        ? IslandTheme.surface
                        : root.uiState.lifecycle === "starting"
                            ? IslandTheme.amberTint
                            : IslandTheme.redTint
                    border.color: root.uiState.lifecycle === "ready"
                        ? IslandTheme.borderStrong : root.uiState.lifecycle === "starting"
                            ? IslandTheme.amber : IslandTheme.red
                    border.width: 1
                    Layout.rightMargin: IslandTheme.pagePadding

                    Label {
                        id: connectionStatus
                        anchors.centerIn: parent
                        text: root.connectionLabel()
                        color: IslandTheme.primaryText
                        font.pixelSize: 11
                        font.weight: Font.Medium
                        elide: Text.ElideMiddle
                    }
                }
            }
        }

        Item {
            id: productionBody
            anchors.top: expandedHeader.bottom
            anchors.left: parent.left
            anchors.right: parent.right
            anchors.bottom: parent.bottom

            Loader {
                id: surfaceLoader
                anchors.fill: parent
                visible: root.semanticOpen
                sourceComponent: root.selectedSurface === "statistics"
                    ? statisticsComponent
                    : root.selectedSurface === "menu" ? menuComponent : usageComponent
            }

            Rectangle {
                id: compactIsland
                objectName: "compact-closed-island"
                anchors.fill: parent
                visible: !root.semanticOpen
                radius: height / 2
                color: IslandTheme.panel
                border.color: IslandTheme.borderStrong
                border.width: 1

                RowLayout {
                    anchors.fill: parent
                    anchors.leftMargin: Kirigami.Units.largeSpacing
                    anchors.rightMargin: Kirigami.Units.largeSpacing
                    spacing: Kirigami.Units.smallSpacing

                    Kirigami.Icon {
                        source: "io.twolab.LlmuxIslands"
                        implicitWidth: Kirigami.Units.iconSizes.small
                        implicitHeight: implicitWidth
                    }

                    Label {
                        Layout.fillWidth: true
                        text: root.compactProviderSummary()
                        color: IslandTheme.primaryText
                        font.bold: root.totalInFlight > 0
                        font.family: IslandTheme.monoFamily
                        font.pixelSize: 11
                        elide: Text.ElideRight
                        horizontalAlignment: Text.AlignHCenter
                    }

                    Label {
                        text: root.uiState.lifecycle === "ready" ? "●" : "○"
                        color: root.uiState.lifecycle === "ready"
                            ? IslandTheme.primaryText
                            : IslandTheme.red
                    }
                }

                MouseArea {
                    id: compactMouseArea
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onEntered: compactHoverOpenTimer.restart()
                    onExited: compactHoverOpenTimer.stop()
                    onClicked: {
                        compactHoverOpenTimer.stop()
                        root.requestOpen("click")
                    }
                }
            }
        }
    }

    Connections {
        target: surfaceLoader.item
        ignoreUnknownSignals: true

        function onDispatchRequested(action, payloadJson) {
            root.routeDispatch(action, payloadJson)
        }
    }

    Component {
        id: usageComponent
        Usage {
            uiState: root.uiState
            advancedVisible: controller.snapshotMode
                && root.snapshotIndex >= 0
                && root.snapshotSurfaces[root.snapshotIndex] === "usage-advanced"
        }
    }

    Component {
        id: statisticsComponent
        Statistics {
            uiState: root.uiState
            advancedVisible: controller.snapshotMode
                && root.snapshotIndex >= 0
                && root.snapshotSurfaces[root.snapshotIndex] === "statistics-advanced"
            receiptSnapshotMode: controller.snapshotMode
                && root.snapshotIndex >= 0
                && root.snapshotSurfaces[root.snapshotIndex] === "receipts"
        }
    }

    Component {
        id: menuComponent
        Menu {
            uiState: root.uiState
            surfaceMode: controller.surfaceMode
            autostartEnabled: controller.autostartEnabled
            advancedVisible: controller.snapshotMode
                && root.snapshotIndex >= 0
                && root.snapshotSurfaces[root.snapshotIndex] === "menu-advanced"
        }
    }

    Timer {
        interval: 10000
        running: !controller.smokeMode && !controller.snapshotMode
        repeat: true
        onTriggered: {
            root.publishScreenInventory()
            root.publishDesktopCapabilities()
            if (root.automaticPollAllowed())
                root.routeDispatch("dashboard_poll", { "source": "poll" })
        }
    }

    Timer {
        id: trayFallbackTimer
        interval: 250
        repeat: false
        onTriggered: root.synchronizeTrayFallback(root.trayAvailable)
    }

    Timer {
        id: startupBootCloseTimer
        interval: 1000
        repeat: false
        onTriggered: root.routeDispatch("boot_close_elapsed", {
            "tray_available": root.trayAvailable && !root.noTrayFallback
        })
    }

    Timer {
        id: snapshotCaptureTimer
        interval: 160
        repeat: false
        onTriggered: root.captureSnapshotSurface()
    }

    Timer {
        interval: 20000
        running: controller.snapshotMode
        repeat: false
        onTriggered: root.failSnapshot(qsTr("Snapshot run timed out"))
    }

    Timer {
        id: compactHoverOpenTimer
        interval: 350
        repeat: false
        onTriggered: {
            if (!root.semanticOpen && compactMouseArea.containsMouse)
                root.requestOpen("hover")
        }
    }

    Timer {
        id: metricsTimer
        interval: 1
        repeat: false
        onTriggered: root.dispatchPreferredWindowMetrics()
    }

    Timer {
        id: focusLossCloseTimer
        interval: 100
        repeat: false
        onTriggered: {
            if (root.semanticOpen && root.openHadActiveFocus && !root.active)
                root.requestClose()
        }
    }

    Shortcut {
        sequence: "Escape"
        context: Qt.ApplicationShortcut
        enabled: root.semanticOpen
        onActivated: root.requestClose()
    }

    Timer {
        interval: 400
        running: controller.smokeMode
        repeat: false
        onTriggered: {
            root.quitRequested = true
            controller.exitHeadless(0)
        }
    }

    Component.onCompleted: {
        publishScreenInventory()
        publishDesktopCapabilities()
        applySelectedScreen()
        // A smoke run proves that the native/QML shell can load. It must not
        // start daemon or dashboard worker threads that can race Qt teardown
        // when the short-lived offscreen process exits.
        if (!controller.snapshotMode && !controller.smokeMode)
            controller.dispatch("app_started", "{}")
    }

    onUiStateChanged: {
        applySelectedScreen()
        metricsTimer.restart()
    }

    onSelectedSurfaceChanged: metricsTimer.restart()
    onPreferredWindowWidthChanged: metricsTimer.restart()
    onPreferredWindowContentHeightChanged: metricsTimer.restart()

    onSurfaceConfiguredChanged: {
        if (surfaceConfigured) {
            applySelectedScreen()
            metricsTimer.restart()
            if (controller.snapshotMode)
                beginSnapshotRun()
            else
                beginStartupPresentation()
        }
    }

    onSemanticOpenChanged: {
        metricsTimer.restart()
        focusLossCloseTimer.stop()
        compactHoverOpenTimer.stop()
        openHadActiveFocus = semanticOpen && active
        if (semanticOpen) {
            publishScreenInventory()
            applySelectedScreen()
            Qt.callLater(function() {
                if (root.semanticOpen) {
                    root.raise()
                    root.requestActivate()
                }
            })
        }
    }

    onActiveChanged: {
        if (active) {
            openHadActiveFocus = semanticOpen
            focusLossCloseTimer.stop()
        } else if (semanticOpen && openHadActiveFocus) {
            focusLossCloseTimer.restart()
        }
    }

    onClosing: function(close) {
        if (!root.quitRequested) {
            close.accepted = false
            root.requestClose()
        }
    }
}
