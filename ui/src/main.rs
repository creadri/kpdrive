//! The kpdrive account and log window.

pub mod bridge;

use cxx_qt_lib::{QGuiApplication, QQmlApplicationEngine, QString, QUrl};

fn main() {
    // Before anything reads a string: the window's labels come from the
    // backend, which reads them from here.
    kpdrive::i18n::init();
    let mut app = QGuiApplication::new();
    // The window icon comes from the desktop entry: Qt uses this as the Wayland
    // app id, and the compositor reads Icon= out of be.otterit.kpdrive.desktop.
    QGuiApplication::set_desktop_file_name(&QString::from("be.otterit.kpdrive"));
    let mut engine = QQmlApplicationEngine::new();
    if let Some(engine) = engine.as_mut() {
        engine.load(&QUrl::from("qrc:/qt/qml/be/otterit/kpdrive/qml/main.qml"));
    }
    if let Some(app) = app.as_mut() {
        app.exec();
    }
}
