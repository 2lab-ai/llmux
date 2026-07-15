//! Thin bridge for QApplication and QWindow integration not yet wrapped by cxx-qt-lib.

use cxx_qt_lib::{QQmlApplicationEngine, QString};
use std::pin::Pin;

#[cxx::bridge(namespace = "llmux_islands")]
mod ffi {
    #[namespace = ""]
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        include!("cxx-qt-lib/qqmlapplicationengine.h");

        type QString = cxx_qt_lib::QString;
        type QQmlApplicationEngine = cxx_qt_lib::QQmlApplicationEngine;
    }

    unsafe extern "C++" {
        include!("qt_runtime.h");

        fn initialize_application();
        fn prepare_surface(mode: &QString);
        fn load_application(engine: Pin<&mut QQmlApplicationEngine>);
        fn configure_surface(engine: Pin<&mut QQmlApplicationEngine>, mode: &QString) -> bool;
        fn exec_application() -> i32;
    }
}

pub fn initialize_application() {
    ffi::initialize_application();
}

pub fn prepare_surface(mode: &str) {
    ffi::prepare_surface(&QString::from(mode));
}

pub fn load_application(engine: Pin<&mut QQmlApplicationEngine>) {
    ffi::load_application(engine);
}

pub fn configure_surface(engine: Pin<&mut QQmlApplicationEngine>, mode: &str) -> bool {
    ffi::configure_surface(engine, &QString::from(mode))
}

pub fn exec_application() -> i32 {
    ffi::exec_application()
}
