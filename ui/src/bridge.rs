//! The QObject behind the window.
//!
//! Anything that touches the network runs on a worker thread with its own tokio
//! runtime; results are posted back onto the Qt thread. Login in particular can
//! sit for minutes waiting on the browser, and the window has to stay alive.

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
        include!("cxx-qt-lib/qstringlist.h");
        type QStringList = cxx_qt_lib::QStringList;
    }

    impl cxx_qt::Threading for Backend {}
    impl cxx_qt::Initialize for Backend {}

    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qproperty(QString, username)]
        #[qproperty(f64, used_bytes, cxx_name = "usedBytes")]
        #[qproperty(f64, total_bytes, cxx_name = "totalBytes")]
        #[qproperty(bool, logged_in, cxx_name = "loggedIn")]
        #[qproperty(bool, busy)]
        #[qproperty(QString, status)]
        #[qproperty(QStringList, log_lines, cxx_name = "logLines")]
        #[qproperty(i32, retention_days, cxx_name = "retentionDays")]
        #[qproperty(QString, sync_folder, cxx_name = "syncFolder")]
        #[qproperty(QString, ignore_file, cxx_name = "ignoreFile")]
        #[qproperty(QString, version)]
        #[qproperty(QString, license)]
        type Backend = super::BackendRust;

        /// Asks the window to open a URL. Routed through QML so Qt can obtain
        /// an activation token and the browser can raise itself.
        #[qsignal]
        #[cxx_name = "openUrlRequested"]
        fn open_url_requested(self: Pin<&mut Self>, url: QString);

        /// Sync somewhere else. Moves what is already synced when it can.
        #[qinvokable]
        #[cxx_name = "changeSyncFolder"]
        fn change_sync_folder(self: Pin<&mut Self>, folder: &QString);

        /// Open the ignore file for editing, writing a commented starter first
        /// if there is none.
        #[qinvokable]
        #[cxx_name = "openIgnoreFile"]
        fn open_ignore_file(self: Pin<&mut Self>);

        /// Reload the account details from Proton.
        #[qinvokable]
        fn refresh(self: Pin<&mut Self>);

        /// Sign in through the browser.
        #[qinvokable]
        fn login(self: Pin<&mut Self>);

        #[qinvokable]
        fn logout(self: Pin<&mut Self>);

        /// Show the log lines containing `term`.
        #[qinvokable]
        #[cxx_name = "searchLogs"]
        fn search_logs(self: Pin<&mut Self>, term: &QString);

        /// Keep this many days of log, and prune what is already past it.
        #[qinvokable]
        #[cxx_name = "setRetention"]
        fn set_retention(self: Pin<&mut Self>, days: i32);

    }
}

use core::pin::Pin;
use cxx_qt::{CxxQtType, Threading};
use cxx_qt_lib::{QString, QStringList};
use std::sync::mpsc::{Sender, channel};

/// What the window asks the worker to do.
enum Task {
    Refresh,
    Login,
    Logout,
}

pub struct BackendRust {
    username: QString,
    used_bytes: f64,
    total_bytes: f64,
    logged_in: bool,
    busy: bool,
    status: QString,
    log_lines: QStringList,
    retention_days: i32,
    sync_folder: QString,
    ignore_file: QString,
    version: QString,
    license: QString,
    tasks: Option<Sender<Task>>,
}

impl Default for BackendRust {
    fn default() -> Self {
        Self {
            username: QString::default(),
            used_bytes: 0.0,
            total_bytes: 0.0,
            logged_in: false,
            busy: false,
            status: QString::from("Starting…"),
            log_lines: QStringList::default(),
            retention_days: 30,
            sync_folder: QString::default(),
            ignore_file: QString::default(),
            version: QString::from(env!("CARGO_PKG_VERSION")),
            license: QString::from("GNU GPL v3 or later"),
            tasks: None,
        }
    }
}

impl cxx_qt::Initialize for qobject::Backend {
    /// Starts the worker thread and loads what can be shown immediately.
    fn initialize(mut self: Pin<&mut Self>) {
        let (tx, rx) = channel::<Task>();
        self.as_mut().rust_mut().tasks = Some(tx);
        let qt = self.as_mut().qt_thread();

        std::thread::spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    let msg = format!("cannot start the background runtime: {e}");
                    let _ = qt.queue(move |mut b| b.as_mut().set_status(QString::from(&msg)));
                    return;
                }
            };
            while let Ok(task) = rx.recv() {
                match task {
                    Task::Refresh => load_account(&runtime, &qt),
                    Task::Login => {
                        let signal = qt.clone();
                        let result = runtime.block_on(kpdrive::account::login(|url, code| {
                            let message = format!("Confirm the code {code} in your browser");
                            let url = url.to_owned();
                            let _ = signal.queue(move |mut b| {
                                b.as_mut().set_status(QString::from(&message));
                                b.as_mut().open_url_requested(QString::from(&url));
                            });
                        }));
                        match result {
                            Ok(_) => load_account(&runtime, &qt),
                            Err(e) => report(&qt, format!("Sign-in failed: {e:#}")),
                        }
                    }
                    Task::Logout => match runtime.block_on(kpdrive::account::logout()) {
                        Ok(()) => {
                            let _ = qt.queue(|mut b| {
                                b.as_mut().set_logged_in(false);
                                b.as_mut().set_username(QString::default());
                                b.as_mut().set_used_bytes(0.0);
                                b.as_mut().set_total_bytes(0.0);
                                b.as_mut().set_busy(false);
                                b.as_mut().set_status(QString::from("Signed out"));
                            });
                        }
                        Err(e) => report(&qt, format!("Sign-out failed: {e:#}")),
                    },
                }
            }
        });

        self.as_mut().set_retention_days(kpdrive::config::load().log_retention_days as i32);
        self.as_mut().show_folder();
        self.as_mut().reload_logs("");
        self.refresh();
    }
}

impl qobject::Backend {
    /// Reads the configured folder into the two path properties.
    fn show_folder(mut self: Pin<&mut Self>) {
        let root = current_root();
        let folder = root.as_ref().map(|r| r.display().to_string()).unwrap_or_default();
        let ignore = root.map(|r| r.join(kpdrive::sync::IGNORE_FILE).display().to_string()).unwrap_or_default();
        self.as_mut().set_sync_folder(QString::from(&folder));
        self.as_mut().set_ignore_file(QString::from(&ignore));
    }

    pub fn change_sync_folder(mut self: Pin<&mut Self>, folder: &QString) {
        let folder = std::path::PathBuf::from(folder.to_string());
        if folder.as_os_str().is_empty() {
            return;
        }
        let mut state = kpdrive::sync::load_state().ok().flatten().unwrap_or_default();
        match kpdrive::sync::set_folder(&mut state, folder) {
            Ok(note) => {
                // A running daemon holds the old path in memory.
                let running = kpdrive::daemon::socket_path().exists();
                let note = if running { format!("{note}. Restart the sync daemon to use it.") } else { note };
                self.as_mut().set_status(QString::from(&note));
            }
            Err(e) => self.as_mut().set_status(QString::from(&format!("cannot change the folder: {e:#}"))),
        }
        self.show_folder();
    }

    pub fn open_ignore_file(mut self: Pin<&mut Self>) {
        let Some(root) = current_root() else {
            self.as_mut().set_status(QString::from("No sync folder yet. Run: kpdrive setup"));
            return;
        };
        if let Err(e) = kpdrive::setup::ignore_template(&root) {
            self.as_mut().set_status(QString::from(&format!("cannot create the ignore file: {e:#}")));
            return;
        }
        let url = format!("file://{}", root.join(kpdrive::sync::IGNORE_FILE).display());
        self.as_mut().open_url_requested(QString::from(&url));
    }

    pub fn refresh(mut self: Pin<&mut Self>) {
        self.as_mut().set_busy(true);
        self.as_mut().set_status(QString::from("Checking the account…"));
        self.send(Task::Refresh);
    }

    pub fn login(mut self: Pin<&mut Self>) {
        self.as_mut().set_busy(true);
        self.as_mut().set_status(QString::from("Opening the browser…"));
        self.send(Task::Login);
    }

    pub fn logout(mut self: Pin<&mut Self>) {
        self.as_mut().set_busy(true);
        self.as_mut().set_status(QString::from("Signing out…"));
        self.send(Task::Logout);
    }

    fn send(self: Pin<&mut Self>, task: Task) {
        if let Some(tx) = &self.rust().tasks {
            let _ = tx.send(task);
        }
    }

    pub fn search_logs(mut self: Pin<&mut Self>, term: &QString) {
        let term = term.to_string();
        self.as_mut().reload_logs(&term);
    }

    /// Log reads are local files and bounded by the line limit, so they run
    /// straight on the UI thread.
    fn reload_logs(mut self: Pin<&mut Self>, term: &str) {
        let mut list = QStringList::default();
        match kpdrive::log::search(term, 2000) {
            Ok(lines) => {
                for line in lines {
                    list.append(QString::from(&line));
                }
            }
            Err(e) => list.append(QString::from(&format!("cannot read the log: {e:#}"))),
        }
        self.as_mut().set_log_lines(list);
    }

    pub fn set_retention(mut self: Pin<&mut Self>, days: i32) {
        let days = days.clamp(1, 3650) as u64;
        // Load and amend: building a fresh Config would drop the sync folder.
        let mut config = kpdrive::config::load();
        config.log_retention_days = days;
        if let Err(e) = kpdrive::config::save(&config) {
            self.as_mut().set_status(QString::from(&format!("cannot save the setting: {e:#}")));
            return;
        }
        match kpdrive::log::prune(days) {
            Ok(0) => {}
            Ok(n) => self.as_mut().set_status(QString::from(&format!("removed {n} log file(s) past {days} days"))),
            Err(e) => self.as_mut().set_status(QString::from(&format!("cannot prune the log: {e:#}"))),
        }
        self.as_mut().set_retention_days(days as i32);
        self.reload_logs("");
    }
}

/// The configured sync folder, falling back to what the sync state recorded.
fn current_root() -> Option<std::path::PathBuf> {
    kpdrive::config::load()
        .sync_folder
        .or_else(|| kpdrive::sync::load_state().ok().flatten().map(|s| s.root))
        .filter(|p| !p.as_os_str().is_empty())
}

/// Loads the account details and pushes them into the window.
fn load_account(runtime: &tokio::runtime::Runtime, qt: &cxx_qt::CxxQtThread<qobject::Backend>) {
    match runtime.block_on(kpdrive::account::info()) {
        Ok(info) => {
            let _ = qt.queue(move |mut b| {
                b.as_mut().set_username(QString::from(&info.username));
                b.as_mut().set_used_bytes(info.used_bytes as f64);
                b.as_mut().set_total_bytes(info.total_bytes as f64);
                b.as_mut().set_logged_in(true);
                b.as_mut().set_busy(false);
                b.as_mut().set_status(QString::default());
            });
        }
        Err(e) => {
            // "not logged in" is the ordinary state before a first sign-in, not a failure.
            let signed_out = e.to_string().contains("not logged in");
            let message = if signed_out { String::new() } else { format!("{e:#}") };
            if !signed_out {
                kpdrive::log::write("ERROR", &format!("account refresh failed: {e:#}"));
            }
            let _ = qt.queue(move |mut b| {
                b.as_mut().set_logged_in(false);
                b.as_mut().set_busy(false);
                b.as_mut().set_status(QString::from(&message));
            });
        }
    }
}

fn report(qt: &cxx_qt::CxxQtThread<qobject::Backend>, message: String) {
    let _ = qt.queue(move |mut b| {
        b.as_mut().set_busy(false);
        b.as_mut().set_status(QString::from(&message));
    });
}
