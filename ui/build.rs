use cxx_qt_build::{CxxQtBuilder, QmlModule};

fn main() {
    CxxQtBuilder::new_qml_module(QmlModule::new("be.otterit.kpdrive").qml_file("qml/main.qml"))
        .files(["src/bridge.rs"])
        .build();
}
