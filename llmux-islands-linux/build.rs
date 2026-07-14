#[cfg(feature = "gui")]
fn main() {
    use cxx_qt_build::{CxxQtBuilder, QmlModule};

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("linux") {
        panic!("the gui feature currently targets Linux desktop systems");
    }
    println!("cargo::rustc-link-lib=LayerShellQtInterface");

    let builder = CxxQtBuilder::new_qml_module(
        QmlModule::new("io.twolab.LlmuxIslands")
            .qml_file("qml/Main.qml")
            .qml_file("qml/Usage.qml")
            .qml_file("qml/Statistics.qml")
            .qml_file("qml/Menu.qml"),
    )
    .qt_module("Network")
    .qt_module("Widgets")
    .files(["src/controller.rs", "src/qt_runtime.rs"])
    .cpp_file("src/qt_runtime.cpp")
    .qrc("resources/icons.qrc");

    // SAFETY: this only adds the crate's source folder to the generated C++ include search path.
    unsafe {
        builder
            .cc_builder(|compiler| {
                compiler.include("src");
            })
            .build();
    }
}

#[cfg(not(feature = "gui"))]
fn main() {}
