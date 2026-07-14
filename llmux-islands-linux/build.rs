#[cfg(feature = "gui")]
fn main() {
    use cxx_qt_build::{CxxQtBuilder, QmlFile, QmlModule};

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("linux") {
        panic!("the gui feature currently targets Linux desktop systems");
    }
    println!("cargo::rustc-link-lib=LayerShellQtInterface");

    let builder = CxxQtBuilder::new_qml_module(
        QmlModule::new("io.twolab.LlmuxIslands")
            .qml_file("qml/Main.qml")
            .qml_file(QmlFile::from("qml/IslandTheme.qml").singleton(true))
            .qml_file("qml/IslandCard.qml")
            .qml_file("qml/IslandButton.qml")
            .qml_file("qml/IslandCheckBox.qml")
            .qml_file("qml/IslandComboBox.qml")
            .qml_file("qml/IslandDialog.qml")
            .qml_file("qml/IslandFieldLabel.qml")
            .qml_file("qml/IslandInlineMessage.qml")
            .qml_file("qml/IslandItemDelegate.qml")
            .qml_file("qml/IslandProgressBar.qml")
            .qml_file("qml/IslandSectionLabel.qml")
            .qml_file("qml/IslandSegmentedControl.qml")
            .qml_file("qml/IslandSeparator.qml")
            .qml_file("qml/IslandSwitch.qml")
            .qml_file("qml/IslandTextArea.qml")
            .qml_file("qml/IslandTextField.qml")
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
