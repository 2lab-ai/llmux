#[cfg(feature = "gui")]
mod controller;
#[cfg(feature = "gui")]
mod qt_runtime;

#[cfg(feature = "gui")]
fn main() {
    use cxx_qt::casting::Upcast;
    use cxx_qt_lib::{QQmlApplicationEngine, QQmlEngine};
    use llmux_islands_core::{DeriveOptions, Presentation};
    use llmux_islands_linux::{
        platform::{detect_surface_mode, SurfaceMode},
        snapshot::{self, SNAPSHOT_NOW_MS},
        ControllerModel,
    };
    use std::env;
    use std::pin::Pin;

    let smoke_mode = env::args_os().any(|argument| argument == "--smoke-test");
    let snapshot_request = match snapshot::request_from_args(env::args_os().skip(1)) {
        Ok(request) => request,
        Err(error) => {
            eprintln!("snapshot launch error: {error}");
            std::process::exit(2);
        }
    };
    if let Some(request) = snapshot_request.as_ref() {
        request.configure_headless_environment();
        let options = DeriveOptions {
            presentation: Presentation::Regular,
            ..DeriveOptions::default()
        };
        if let Err(error) = ControllerModel::from_fixture(options, SNAPSHOT_NOW_MS) {
            eprintln!("snapshot fixture error: {error}");
            std::process::exit(2);
        }
    }
    let headless_run = smoke_mode || snapshot_request.is_some();
    if let Err(error) = snapshot::configure(snapshot_request) {
        eprintln!("snapshot launch error: {error}");
        std::process::exit(2);
    }

    let surface_mode = if snapshot::active().is_some() {
        SurfaceMode::RegularWindow
    } else {
        detect_surface_mode()
    };
    // CXX-Qt links the generated QML plugin and resource collection statically.
    // Force their registration before the engine attempts to resolve Main.qml.
    cxx_qt::init_crate!(llmux_islands_linux);
    qt_runtime::prepare_surface(surface_mode.as_str());
    qt_runtime::initialize_application();

    let mut engine = QQmlApplicationEngine::new();

    if let Some(engine) = engine.as_mut() {
        qt_runtime::load_application(engine);
    }

    if let Some(mut engine) = engine.as_mut() {
        if !qt_runtime::configure_surface(engine.as_mut(), surface_mode.as_str()) {
            eprintln!("failed to configure the root QML window");
            std::process::exit(1);
        }
        let engine: Pin<&mut QQmlEngine> = engine.upcast_pin();
        engine.on_quit(|_| {}).release();
    }

    let exit_code = qt_runtime::exec_application();
    // Qt's offscreen platform can fault while destroying native/QML objects
    // after a successful short-lived run. The event-loop result is the
    // headless contract, and snapshot files have already been flushed before
    // QML exits, so avoid the unreliable platform-plugin teardown only for
    // explicit smoke/snapshot processes. Live desktop sessions still unwind.
    if headless_run {
        snapshot::exit_immediately(exit_code);
    }
    if exit_code != 0 {
        std::process::exit(exit_code);
    }
}

#[cfg(not(feature = "gui"))]
fn main() {
    eprintln!("llmux-islands-linux was built without the `gui` feature");
}
