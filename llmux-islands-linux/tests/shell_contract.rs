use std::{fs, path::PathBuf};

fn crate_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn read(relative: &str) -> String {
    fs::read_to_string(crate_path(relative))
        .unwrap_or_else(|error| panic!("missing shell contract resource {relative}: {error}"))
}

#[test]
fn qml_shell_exposes_every_primary_surface() {
    let main = read("qml/Main.qml");
    assert!(main.contains("IslandsController"));

    for (file, marker) in [
        ("qml/Usage.qml", "Usage"),
        ("qml/Statistics.qml", "Statistics"),
        ("qml/Menu.qml", "Menu"),
    ] {
        let source = read(file);
        assert!(
            source.contains(marker),
            "{file} must identify its primary surface"
        );
    }
}

#[test]
fn qml_visual_system_uses_the_inverted_openai_reference_without_system_chrome() {
    let theme = read("qml/IslandTheme.qml");
    for marker in [
        r##"panel: "#000000""##,
        r##"surface: "#000000""##,
        r##"primaryText: "#ffffff""##,
        "secondaryText: Qt.rgba(1, 1, 1, 0.60)",
        "tertiaryText: Qt.rgba(1, 1, 1, 0.50)",
        "disabledText: Qt.rgba(1, 1, 1, 0.44)",
        r##"focus: "#ffffff""##,
        r##"green: "#66bf73""##,
        r##"amber: "#ffb300""##,
        r##"red: "#ff4d4d""##,
        "cardRadius: 0",
        "controlRadius: 0",
        "providerAccent(provider)",
        "quotaAccent(remaining, constraining, warningLevel)",
        r#"monoFamily: "monospace""#,
    ] {
        assert!(
            theme.contains(marker),
            "missing inverted OpenAI token {marker}"
        );
    }
    for forbidden in ["blue:", "blueTint:", "magenta:", "cyan:"] {
        assert!(
            !theme.contains(forbidden),
            "normal visual tokens must not carry chromatic accent {forbidden}"
        );
    }

    let build = read("build.rs");
    assert!(
        build.contains(r#"QmlFile::from("qml/IslandTheme.qml").singleton(true)"#),
        "IslandTheme must be registered as a real QML singleton"
    );
    for component in [
        "IslandCard.qml",
        "IslandButton.qml",
        "IslandTextField.qml",
        "IslandComboBox.qml",
        "IslandItemDelegate.qml",
        "IslandSwitch.qml",
        "IslandProgressBar.qml",
        "IslandSegmentedControl.qml",
        "IslandDialog.qml",
        "IslandFieldLabel.qml",
    ] {
        assert!(
            build.contains(&format!(r#".qml_file("qml/{component}")"#)),
            "QML module must register {component}"
        );
    }

    let main = read("qml/Main.qml");
    for marker in [
        "color: semanticOpen ? IslandTheme.panel",
        "palette.window: IslandTheme.panel",
        "IslandSegmentedControl",
        "color: IslandTheme.panel",
        "border.color: IslandTheme.border",
    ] {
        assert!(main.contains(marker), "dark shell must contain {marker}");
    }

    for page in ["qml/Usage.qml", "qml/Statistics.qml", "qml/Menu.qml"] {
        let source = read(page);
        for marker in [
            "padding: IslandTheme.pagePadding",
            "background: Rectangle { color: IslandTheme.panel }",
            "palette.windowText: IslandTheme.primaryText",
        ] {
            assert!(source.contains(marker), "{page} must contain {marker}");
        }
        for forbidden in [
            "Kirigami.AbstractCard",
            "Kirigami.InlineMessage",
            "Kirigami.Separator",
            "Kirigami.Theme.",
        ] {
            assert!(
                !source.contains(forbidden),
                "{page} must not leak bright system chrome through {forbidden}"
            );
        }
    }
}

#[test]
fn production_surfaces_use_provider_quota_segment_and_receipt_hierarchy() {
    let usage = read("qml/Usage.qml");
    for marker in [
        "IslandTheme.providerAccent(accountCard.account.provider)",
        "IslandTheme.quotaAccent(",
        "IslandProgressBar",
        "accentColor: usagePage.gaugeColor(",
        "IslandSegmentedControl",
    ] {
        assert!(usage.contains(marker), "Usage must contain {marker}");
    }

    let statistics = read("qml/Statistics.qml");
    for marker in [
        "property int selectedSection: 0",
        "IslandSegmentedControl",
        "IslandSectionLabel",
        "font.family: IslandTheme.monoFamily",
        "id: receiptEvidenceSection",
    ] {
        assert!(
            statistics.contains(marker),
            "Statistics must contain {marker}"
        );
    }

    let menu = read("qml/Menu.qml");
    for marker in [
        "IslandCard",
        "IslandButton",
        "IslandComboBox",
        "IslandSegmentedControl",
        "IslandSwitch",
        "IslandTextField",
        "IslandDialog",
        "font.family: IslandTheme.monoFamily",
    ] {
        assert!(menu.contains(marker), "Menu must contain {marker}");
    }
}

#[test]
fn offscreen_snapshot_controls_keep_labels_explicit_and_account_actions_discoverable() {
    let field_label = read("qml/IslandFieldLabel.qml");
    for marker in [
        "color: IslandTheme.secondaryText",
        "font.weight: Font.Medium",
        "horizontalAlignment: Text.AlignRight",
    ] {
        assert!(
            field_label.contains(marker),
            "dark form label must contain {marker}"
        );
    }
    assert!(
        !field_label.contains("font.family: IslandTheme.monoFamily"),
        "prose form labels must use the system grotesque"
    );

    let menu = read("qml/Menu.qml");
    assert!(
        menu.matches("IslandFieldLabel").count() >= 18,
        "every production settings row needs an explicit visible field label"
    );
    for forbidden in ["Kirigami.FormLayout", "Kirigami.FormData"] {
        assert!(
            !menu.contains(forbidden),
            "offscreen menu must not inherit invisible system form chrome through {forbidden}"
        );
    }

    let usage = read("qml/Usage.qml");
    for marker in [
        "id: accountActionsButton",
        "text: \"⋯\"",
        "display: AbstractButton.TextOnly",
        "Accessible.name: qsTr(\"Account actions\")",
        "visible: usagePage.advancedVisible",
    ] {
        assert!(
            usage.contains(marker),
            "account action fallback must contain {marker}"
        );
    }
    assert!(
        !usage.contains("icon.name: \"overflow-menu\""),
        "account actions must not depend on an offscreen icon theme"
    );
}

#[test]
fn qml_pages_normalize_qt_variant_lists_before_using_array_methods() {
    for page in ["qml/Usage.qml", "qml/Statistics.qml", "qml/Menu.qml"] {
        let source = read(page);
        for marker in [
            "function arrayLikeLength(value)",
            "var length = Number(value.length)",
            "Math.floor(length) === length",
            "result.push(value[index])",
            "arrayLikeLength(value) < 0",
        ] {
            assert!(
                source.contains(marker),
                "{page} must normalize Qt QVariantList values through {marker}"
            );
        }
    }
}

#[test]
fn controller_contract_is_qml_invokable() {
    let controller = read("src/controller.rs");
    assert!(controller.contains("#[auto_cxx_name]"));
    assert!(controller.contains("qproperty(QString, state_json"));
    assert!(controller.contains("fn dispatch"));
    assert!(controller.contains("action: &QString"));
    assert!(controller.contains("payload_json: &QString"));

    let main = read("src/main.rs");
    assert!(main.contains("cxx_qt::init_crate!(llmux_islands_linux)"));
}

#[test]
fn deterministic_fixture_contains_request_receipts() {
    let fixture = read("fixtures/dashboard.json");
    let macos_fixture = read("../llmux-islands/LlmuxIslands/Resources/snapshot-dashboard.json");
    assert_eq!(
        fixture, macos_fixture,
        "macOS and KDE renderer evidence must consume byte-identical dashboard fixtures"
    );
    for marker in [
        r#""current_by_group""#,
        r#""accounts""#,
        r#""model_usage""#,
        r#""activity""#,
        r#""completed""#,
    ] {
        assert!(fixture.contains(marker), "fixture must contain {marker}");
    }
}

#[test]
fn canonical_controller_runtime_is_async_and_timer_bounded() {
    let cargo = read("Cargo.toml");
    assert!(cargo.contains("llmux-islands-core"));

    let controller = read("src/controller.rs");
    for marker in [
        "impl cxx_qt::Threading for IslandsController",
        "qt_thread()",
        "std::thread::spawn",
        "OperationFinished",
        "execute_maintenance",
        "LoginStatus::CancellationFailed",
    ] {
        assert!(
            controller.contains(marker),
            "controller runtime must contain {marker}"
        );
    }
    assert!(controller.contains("snapshot::active()"));
    assert!(controller.contains("if snapshot_request.is_some()"));
    assert!(controller.contains("ControllerModel::from_fixture(options, SNAPSHOT_NOW_MS)"));
    assert!(controller.contains("ControllerModel::new(options)"));
    assert!(
        !controller.contains("RefreshSource::Mutation"),
        "failed connection persistence must not fetch through the old client"
    );

    let main = read("qml/Main.qml");
    for marker in [
        "uiState.navigation",
        "uiState.connection",
        "interval: 10000",
        "onDispatchRequested",
        "controller.dispatch(\"app_started\"",
    ] {
        assert!(
            main.contains(marker),
            "canonical shell must contain {marker}"
        );
    }
    assert!(
        !main.contains("connection.endpoint_display"),
        "the default shell status must not expose the technical daemon endpoint"
    );
    assert!(!main.contains("uiState.selected_surface"));
}

#[test]
fn arch_and_desktop_packaging_resources_exist() {
    for relative in [
        "resources/io.twolab.LlmuxIslands.desktop",
        "resources/io.twolab.LlmuxIslands.metainfo.xml",
        "resources/icons/io.twolab.LlmuxIslands.svg",
        "packaging/arch/Dockerfile",
    ] {
        assert!(crate_path(relative).is_file(), "missing {relative}");
    }
}

#[test]
fn kde_tray_contract_wires_activation_refresh_and_quit() {
    let main = read("qml/Main.qml");
    for marker in [
        "Platform.SystemTrayIcon",
        "id: trayLoader",
        "active: !controller.smokeMode && !controller.snapshotMode",
        "surfaceConfigured",
        "trayFallbackTimer",
        "onActivated",
        "tray_activated",
        "onMessageClicked",
        "requestOpen(\"notification\")",
        "showMessage(",
        r#"routeDispatch("refresh_requested""#,
        "Qt.quit()",
        "providerInFlightSummary(true)",
        "Total: %2 in flight",
        "trayStatusText()",
        "trayNeedsAttention",
        "dialog-warning",
        "icon.source: root.trayNeedsAttention",
        "? \"\" : \"qrc:/icons/io.twolab.LlmuxIslands.svg\"",
        "Account health needs attention",
        "publishDesktopCapabilities",
        "desktop_capabilities_changed",
        "root.publishDesktopCapabilities()",
    ] {
        assert!(main.contains(marker), "tray shell must contain {marker}");
    }
}

#[test]
fn layer_shell_has_a_compact_semantic_closed_and_dynamic_open_surface() {
    let main = read("qml/Main.qml");
    for marker in [
        "objectName: \"compact-closed-island\"",
        "controller.surfaceMode === \"wayland-layer-shell\"",
        "requestOpen(\"click\")",
        "compactHoverOpenTimer",
        "requestOpen(\"hover\")",
        "open_requested",
        "close_requested",
        "window_metrics_changed",
        "windowState.width",
        "windowState.content_height",
        "preferredContentHeight",
        "sequence: \"Escape\"",
        "onActiveChanged",
    ] {
        assert!(
            main.contains(marker),
            "semantic window shell must contain {marker}"
        );
    }
    for forbidden in ["Behavior on width", "Behavior on height", "NumberAnimation"] {
        assert!(
            !main.contains(forbidden),
            "frequent open/close geometry must not animate through {forbidden}"
        );
    }
    assert!(!main.contains("width: 920"));
    assert!(!main.contains("height: 720"));

    for page in ["qml/Usage.qml", "qml/Statistics.qml", "qml/Menu.qml"] {
        assert!(
            read(page).contains("preferredContentHeight"),
            "{page} must expose dynamic content height"
        );
    }
}

#[test]
fn normal_startup_presents_boot_for_one_second_without_closing_no_tray_fallback() {
    let main = read("qml/Main.qml");
    for marker in [
        "function beginStartupPresentation",
        "requestOpen(\"boot\")",
        "id: startupBootCloseTimer",
        "interval: 1000",
        "boot_close_elapsed",
        "root.trayAvailable && !root.noTrayFallback",
        "root.synchronizeTrayFallback(root.trayAvailable)",
    ] {
        assert!(
            main.contains(marker),
            "startup boot shell must contain {marker}"
        );
    }
}

#[test]
fn tray_fallback_tracks_runtime_availability_without_a_qt_change_signal() {
    let main = read("qml/Main.qml");
    for marker in [
        "function synchronizeTrayFallback(trayAvailable)",
        "synchronizeTrayFallback(trayAvailable)",
        "var fallbackRequired = !trayAvailable",
        "noTrayFallback = fallbackRequired",
        "fallbackRequired && !semanticOpen",
        "root.publishDesktopCapabilities()",
    ] {
        assert!(
            main.contains(marker),
            "dynamic tray fallback must contain {marker}"
        );
    }
}

#[test]
fn explicit_snapshot_cli_renders_full_surfaces_and_a_receipt_detail() {
    let main = read("src/main.rs");
    let controller = read("src/controller.rs");
    let qml = read("qml/Main.qml");
    let docker = read("packaging/arch/Dockerfile");

    for marker in [
        "snapshot::request_from_args",
        "configure_headless_environment",
        "ControllerModel::from_fixture(options, SNAPSHOT_NOW_MS)",
        "let headless_run = smoke_mode || snapshot_request.is_some()",
        "snapshot::exit_immediately(exit_code)",
    ] {
        assert!(main.contains(marker), "snapshot CLI must contain {marker}");
    }
    for marker in [
        "qproperty(bool, snapshot_mode)",
        "qproperty(QString, snapshot_dir)",
        "fn exit_headless(self: Pin<&mut Self>, exit_code: i32)",
        "fn fail_headless(self: Pin<&mut Self>, message: &QString)",
        "snapshot::exit_immediately(exit_code)",
        "LocalSettings::default()",
        "snapshot-fixture",
    ] {
        assert!(
            controller.contains(marker),
            "snapshot controller must contain {marker}"
        );
    }
    for marker in [
        "snapshotSurfaces: [\"usage\", \"usage-advanced\", \"statistics\", \"statistics-advanced\", \"receipts\", \"menu\", \"menu-advanced\"]",
        "snapshotTarget.grabToImage(saveResult, Qt.size(960, height))",
        "result.saveToFile(outputPath)",
        "controller.failHeadless(message)",
        "controller.exitHeadless(0)",
        "if (!controller.snapshotMode && !controller.smokeMode)",
        "running: !controller.smokeMode && !controller.snapshotMode",
        "Snapshot run timed out",
        "snapshotCaptureAttempts >= 50",
        "function retrySnapshot(message)",
        "function snapshotPreferredContentHeight()",
        "Math.ceil(pageHeight) + headerHeight + 32",
        "function snapshotRoute(name)",
        "name === \"usage-advanced\"",
        "name === \"statistics-advanced\" || name === \"receipts\"",
        "name === \"menu-advanced\"",
        "function snapshotSurfaceCount(name)",
        "function snapshotSurfaceFlag(name)",
        "surface === \"usage-advanced\" || surface === \"statistics-advanced\"",
        "surface === \"usage\" || surface === \"usage-advanced\"",
        "snapshotSurfaceFlag(\"advancedVisible\")",
        "snapshotSurfaceCount(\"renderedGaugeCount\") < 1",
        "snapshotSurfaceCount(\"renderedHeatmapCellCount\") < 1",
        "snapshotSurfaceCount(\"renderedServingAccountCount\") < 1",
        "snapshotSurfaceCount(\"renderedVerificationReceiptCount\") < 1",
        "id: snapshotTarget",
        "id: expandedHeader",
        "id: productionBody",
        "anchors.top: expandedHeader.bottom",
        "root.snapshotSurfaces[root.snapshotIndex] === \"usage-advanced\"",
        "root.snapshotSurfaces[root.snapshotIndex] === \"statistics-advanced\"",
        "receiptSnapshotMode: controller.snapshotMode",
        "root.snapshotSurfaces[root.snapshotIndex] === \"receipts\"",
        "root.snapshotSurfaces[root.snapshotIndex] === \"menu-advanced\"",
    ] {
        assert!(
            qml.contains(marker),
            "snapshot runtime must contain {marker}"
        );
    }
    assert_eq!(
        qml.matches("id: expandedHeader").count(),
        1,
        "snapshot and production must reuse one real header"
    );
    assert!(
        !qml.contains("header: ToolBar"),
        "the real production header must live inside the shell capture target"
    );
    assert!(
        !qml.contains("surfaceLoader.item.snapshotReceiptTarget"),
        "receipt evidence must capture its exact user-facing Statistics route with shell chrome"
    );
    assert!(
        !qml.contains("if (name === \"receipts\")"),
        "receipt evidence must size the complete Statistics route instead of a detached detail"
    );
    for marker in [
        "--snapshot-dir",
        "89504e470d0a1a0a",
        "000003c0*",
        "test \"$size\" -gt 20000",
        "receipts.png",
        "sha256sum",
        "SHA256SUMS",
        "QML warning:",
        "./target/debug/llmux-islands-linux --smoke-test",
        "./target/debug/llmux-islands-linux --snapshot-dir",
    ] {
        assert!(
            docker.contains(marker),
            "Arch snapshot receipt must contain {marker}"
        );
    }
    assert!(
        !docker.contains("cargo run --locked -- --smoke-test"),
        "Arch smoke verification must execute the binary built in the prior layer"
    );
}

#[test]
fn t6_advanced_disclosure_is_local_monochrome_and_keeps_common_failures_visible() {
    let usage = read("qml/Usage.qml");
    let statistics = read("qml/Statistics.qml");
    let menu = read("qml/Menu.qml");
    let main = read("qml/Main.qml");

    for (source, object_name, local_toggle) in [
        (
            &usage,
            "usage-advanced-disclosure",
            "onClicked: usagePage.advancedVisible = checked",
        ),
        (
            &statistics,
            "statistics-advanced-disclosure",
            "onClicked: statisticsPage.advancedVisible = checked",
        ),
        (
            &menu,
            "menu-advanced-disclosure",
            "onClicked: menuPage.advancedVisible = checked",
        ),
    ] {
        assert!(source.contains("property bool advancedVisible: false"));
        assert!(source.contains(&format!("objectName: \"{object_name}\"")));
        assert!(source.contains("text: qsTr(\"Advanced\")"));
        assert!(source.contains("checkable: true"));
        assert!(source.contains(local_toggle));

        let disclosure = source
            .split(&format!("objectName: \"{object_name}\""))
            .nth(1)
            .and_then(|tail| tail.split("            }").next())
            .expect("advanced disclosure block");
        assert!(
            !disclosure.contains("dispatchRequested"),
            "Advanced toggles presentation only"
        );
    }

    for marker in [
        "id: providerCounterFlow",
        "visible: usagePage.advancedVisible && providerCounterRepeater.count > 0",
        "model: usagePage.advancedVisible",
        "visible: usagePage.visibleUsageReceipts().length > 0",
        "objectName: \"usage-offline-state\"",
        "accountCard.account.healthy === false",
        "accountCard.tokenExpiry.state === \"expired\"",
        "return verificationReceipts.filter",
    ] {
        assert!(
            usage.contains(marker),
            "Usage T6 contract must contain {marker}"
        );
    }

    for marker in [
        "objectName: \"statistics-account-overview\"",
        "visible: statisticsPage.effectiveAdvancedVisible",
        "property bool receiptSnapshotMode: false",
        "advancedVisible || receiptSnapshotMode",
        "visible: statisticsPage.receiptSnapshotMode",
        "healthSummaryReason",
    ] {
        assert!(
            statistics.contains(marker),
            "Statistics T6 contract must contain {marker}"
        );
    }

    for marker in [
        "objectName: \"menu-connection-attention\"",
        "visible: menuPage.connectionNeedsAttention()",
        "visible: menuPage.advancedVisible",
        "objectName: \"connection-settings\"",
        "objectName: \"platform-diagnostics\"",
        "objectName: \"events-settings\"",
        "objectName: \"maintenance-settings\"",
        "objectName: \"about-llmux-islands\"",
        "failedReceipts(menuReceiptItems)",
        "Launch Islands at login",
        "Anonymize account email",
    ] {
        assert!(
            menu.contains(marker),
            "Settings T6 contract must contain {marker}"
        );
    }

    assert!(main.contains("model: [qsTr(\"Usage\"), qsTr(\"Statistics\"), qsTr(\"Settings\")]"));
    assert!(main.contains("index === 2 ? \"menu\" : \"usage\""));

    for source in [&usage, &statistics, &menu] {
        assert!(!source.contains("IslandTheme.blue"));
        assert!(!source.contains("gradient"));
        assert!(!source.contains("shadow"));
    }
}

#[test]
fn automatic_dashboard_poll_honors_the_semantic_retry_deadline() {
    let main = read("qml/Main.qml");
    for marker in [
        "function automaticPollAllowed",
        "connection.retry_at_ms",
        "Date.now() >= retryAt",
        "if (root.automaticPollAllowed())",
    ] {
        assert!(
            main.contains(marker),
            "retry-aware poller must contain {marker}"
        );
    }
    assert!(main.contains(r#"routeDispatch("refresh_requested""#));
}

#[test]
fn qt_tray_owns_notification_ui_while_canberra_preserves_sound() {
    let main = read("qml/Main.qml");
    let controller = read("src/controller.rs");

    for marker in [
        "show_notification",
        "trayLoader.item",
        "tray.showMessage",
        "onMessageClicked",
    ] {
        assert!(
            main.contains(marker),
            "native notification shell must contain {marker}"
        );
    }
    for marker in [
        "emit_notification",
        "show_notification",
        "spawn_notification_sound",
        "canberra-gtk-play",
    ] {
        assert!(
            controller.contains(marker),
            "notification controller must contain {marker}"
        );
    }
    assert!(
        !controller.contains("FreedesktopNotifier"),
        "the controller must not emit a duplicate notify-send notification"
    );
}

#[test]
fn native_surface_contract_has_wayland_x11_and_regular_paths() {
    let native = read("src/qt_runtime.cpp");
    for marker in [
        "QT_VERSION < QT_VERSION_CHECK(6, 5, 0)",
        "LayerShellQt::Window::get",
        "LayerShellQt::Window::LayerOverlay",
        "LayerShellQt::Window::AnchorTop",
        "setDesktopFileName",
        "position_x11_window",
        "regular-window",
    ] {
        assert!(
            native.contains(marker),
            "native surface host must contain {marker}"
        );
    }
    assert!(
        !native.contains("window->show();"),
        "native surface setup must preserve tray-hidden QML visibility"
    );
}

#[test]
fn real_qt_screens_feed_settings_and_apply_to_the_window() {
    let main = read("qml/Main.qml");
    for marker in [
        "Application.screens",
        "screen_inventory_changed",
        "root.screen = target",
        "semanticWindow.selected_screen_id",
    ] {
        assert!(
            main.contains(marker),
            "screen integration must contain {marker}"
        );
    }
}

#[test]
fn desktop_adapters_and_arch_package_are_present() {
    let desktop = read("src/desktop.rs");
    for marker in [
        "AutostartManager",
        "ensure_sibling_daemon",
        "is_loopback_endpoint",
        "FreedesktopNotifier",
        "canberra-gtk-play",
    ] {
        assert!(
            desktop.contains(marker),
            "desktop adapter must contain {marker}"
        );
    }

    let pkgbuild = read("packaging/arch/PKGBUILD");
    for marker in [
        "package()",
        "usr/bin/llmux-islands-linux",
        "usr/share/applications",
        "usr/share/metainfo",
        "usr/share/icons/hicolor/scalable/apps",
        "options=('!lto')",
    ] {
        assert!(pkgbuild.contains(marker), "PKGBUILD must contain {marker}");
    }

    let docker = read("packaging/arch/Dockerfile");
    for marker in [
        "USER builder",
        "makepkg --cleanbuild",
        "pacman -U --noconfirm",
        "/usr/bin/llmux-islands-linux --smoke-test",
        "FROM scratch AS evidence",
    ] {
        assert!(docker.contains(marker), "package CI must contain {marker}");
    }

    assert_eq!(read(".gitignore"), "/target/\n");
}
