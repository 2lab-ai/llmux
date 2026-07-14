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
        "connection.endpoint_display",
        "interval: 10000",
        "onDispatchRequested",
        "controller.dispatch(\"app_started\"",
    ] {
        assert!(
            main.contains(marker),
            "canonical shell must contain {marker}"
        );
    }
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
        "Behavior on width",
        "Behavior on height",
        "duration: 140",
    ] {
        assert!(
            main.contains(marker),
            "semantic window shell must contain {marker}"
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
        "snapshotSurfaces: [\"usage\", \"statistics\", \"receipts\", \"menu\"]",
        "captureTarget.grabToImage",
        "result.saveToFile(outputPath)",
        "controller.exitHeadless(2)",
        "controller.exitHeadless(0)",
        "if (!controller.snapshotMode && !controller.smokeMode)",
        "running: !controller.smokeMode && !controller.snapshotMode",
        "Snapshot run timed out",
        "snapshotCaptureAttempts >= 50",
        "function snapshotPreferredContentHeight()",
        "Math.ceil(pageHeight) + headerHeight + 32",
        "function snapshotSurfaceCount(name)",
        "snapshotSurfaceCount(\"renderedGaugeCount\") < 1",
        "snapshotSurfaceCount(\"renderedHeatmapCellCount\") < 1",
        "snapshotSurfaceCount(\"renderedServingAccountCount\") < 1",
        "snapshotSurfaceCount(\"renderedVerificationReceiptCount\") < 1",
        "surfaceLoader.item.snapshotReceiptTarget",
    ] {
        assert!(
            qml.contains(marker),
            "snapshot runtime must contain {marker}"
        );
    }
    for marker in [
        "--snapshot-dir",
        "89504e470d0a1a0a",
        "000003c0*",
        "test \"$size\" -gt 20000",
        "receipts.png",
        "sha256sum",
        "SHA256SUMS",
        "QML warning:",
    ] {
        assert!(
            docker.contains(marker),
            "Arch snapshot receipt must contain {marker}"
        );
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
