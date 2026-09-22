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
        #[qproperty(QString, log_level, cxx_name = "logLevel")]
        #[qproperty(bool, sync_photos, cxx_name = "syncPhotos")]
        #[qproperty(QString, photos_folder, cxx_name = "photosFolder")]
        #[qproperty(QString, sync_status, cxx_name = "syncStatus")]
        #[qproperty(bool, sync_busy, cxx_name = "syncBusy")]
        #[qproperty(bool, daemon_running, cxx_name = "daemonRunning")]
        #[qproperty(bool, sync_failed, cxx_name = "syncFailed")]
        #[qproperty(bool, sync_offline, cxx_name = "syncOffline")]
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
        /// `choice` answers [`folderQuestion`]: "merge" or "rename".
        #[qinvokable]
        #[cxx_name = "changeSyncFolder"]
        fn change_sync_folder(self: Pin<&mut Self>, folder: &QString, choice: &QString);

        /// What to ask before syncing into `folder`; empty when nothing needs asking.
        #[qinvokable]
        #[cxx_name = "folderQuestion"]
        fn folder_question(self: Pin<&mut Self>, folder: &QString) -> QString;

        /// The answers to that question, worded as the CLI words them.
        #[qinvokable]
        #[cxx_name = "folderChoices"]
        fn folder_choices(self: Pin<&mut Self>) -> QStringList;

        /// Open the ignore file for editing, writing a commented starter first
        /// if there is none.
        #[qinvokable]
        #[cxx_name = "openIgnoreFile"]
        fn open_ignore_file(self: Pin<&mut Self>);

        /// Turn the Proton Photos download on or off. Named apart from the
        /// property's own generated setter, which would otherwise clash.
        #[qinvokable]
        #[cxx_name = "changeSyncPhotos"]
        fn change_sync_photos(self: Pin<&mut Self>, on: bool);

        /// Ask the sync daemon what it is doing. Cheap: a local socket.
        #[qinvokable]
        #[cxx_name = "refreshSyncStatus"]
        fn refresh_sync_status(self: Pin<&mut Self>);

        /// Tell the daemon to sync now.
        #[qinvokable]
        #[cxx_name = "syncNow"]
        fn sync_now(self: Pin<&mut Self>);

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

        /// Store only lines of this level or worse. Named apart from the
        /// property's own generated setter, which would otherwise clash.
        #[qinvokable]
        #[cxx_name = "changeLogLevel"]
        fn change_log_level(self: Pin<&mut Self>, level: &QString);

    }
}

use core::pin::Pin;
use cxx_qt::{CxxQtType, Threading};
use cxx_qt_lib::{QString, QStringList};
use std::sync::mpsc::{Sender, channel};

/// How many log lines the window shows. The whole log is laid out at once so
/// a selection can cross lines, so this is also what bounds what that costs.
const LOG_LINES: usize = 100;

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
    log_level: QString,
    sync_photos: bool,
    photos_folder: QString,
    sync_status: QString,
    sync_busy: bool,
    daemon_running: bool,
    sync_failed: bool,
    sync_offline: bool,
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
            log_level: QString::from("WARN"),
            sync_photos: false,
            photos_folder: QString::default(),
            sync_status: QString::default(),
            sync_busy: false,
            daemon_running: false,
            sync_failed: false,
            sync_offline: false,
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
                            Ok(_) => {
                                // The daemon is still holding the session this
                                // sign-in replaced; it reloads when poked.
                                kpdrive::daemon::poke();
                                load_account(&runtime, &qt);
                            }
                            Err(e) => report(&qt, format!("Sign-in failed: {e:#}")),
                        }
                    }
                    Task::Logout => match runtime.block_on(kpdrive::account::logout()) {
                        Ok(()) => {
                            kpdrive::daemon::poke();
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

        let config = kpdrive::config::load();
        self.as_mut().set_retention_days(config.log_retention_days as i32);
        self.as_mut().set_log_level(QString::from(config.log_level.name()));
        self.as_mut().set_sync_photos(config.sync_photos);
        let photos = kpdrive::photos::load_state()
            .ok()
            .flatten()
            .map(|s| s.dest)
            .filter(|d| !d.as_os_str().is_empty())
            .or_else(|| kpdrive::photos::default_dest().ok())
            .map(|d| d.display().to_string())
            .unwrap_or_default();
        self.as_mut().set_photos_folder(QString::from(&photos));
        self.as_mut().show_folder();
        self.as_mut().refresh_sync_status();
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

    /// The question to put before syncing into `folder`, empty when there is
    /// nothing to ask. The wording comes from the same place the CLI reads it.
    pub fn folder_question(self: Pin<&mut Self>, folder: &QString) -> QString {
        let folder = std::path::PathBuf::from(folder.to_string());
        let state = kpdrive::sync::load_state().ok().flatten().unwrap_or_default();
        QString::from(&kpdrive::sync::folder_question(&state, &folder).unwrap_or_default())
    }

    /// The two answers, in the order the window should offer them.
    pub fn folder_choices(self: Pin<&mut Self>) -> QStringList {
        let mut list = QStringList::default();
        list.append(QString::from(kpdrive::sync::Occupied::Merge.label()));
        list.append(QString::from(kpdrive::sync::Occupied::Rename.label()));
        list
    }

    pub fn change_sync_folder(mut self: Pin<&mut Self>, folder: &QString, choice: &QString) {
        let folder = std::path::PathBuf::from(folder.to_string());
        if folder.as_os_str().is_empty() {
            return;
        }
        let mut state = kpdrive::sync::load_state().ok().flatten().unwrap_or_default();
        let occupied = match choice.to_string().as_str() {
            "rename" => kpdrive::sync::Occupied::Rename,
            _ => kpdrive::sync::Occupied::Merge,
        };
        match kpdrive::sync::set_folder(&mut state, folder, occupied) {
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

    /// The daemon's own account of itself, in the words `kpdrive status` uses.
    pub fn refresh_sync_status(mut self: Pin<&mut Self>) {
        let report = kpdrive::daemon::ask();
        self.as_mut().set_daemon_running(report.running);
        self.as_mut().set_sync_busy(report.syncing);
        // An outage clears itself, so it reads as something to wait out
        // rather than something that went wrong.
        self.as_mut().set_sync_offline(report.offline);
        self.as_mut().set_sync_failed(report.signed_out || (report.error.is_some() && !report.offline));
        self.as_mut().set_sync_status(QString::from(&report.sentence()));
    }

    pub fn sync_now(self: Pin<&mut Self>) {
        kpdrive::daemon::poke();
        self.refresh_sync_status();
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
    /// straight on the UI thread. The window shows this run's newest
    /// [`LOG_LINES`], newest first.
    fn reload_logs(mut self: Pin<&mut Self>, term: &str) {
        let mut list = QStringList::default();
        match kpdrive::log::session(term, LOG_LINES) {
            Ok(lines) => {
                for line in lines {
                    list.append(QString::from(&line));
                }
            }
            Err(e) => list.append(QString::from(&format!("cannot read the log: {e:#}"))),
        }
        self.as_mut().set_log_lines(list);
    }

    pub fn change_sync_photos(mut self: Pin<&mut Self>, on: bool) {
        // Load and amend: building a fresh Config would drop the other settings.
        let mut config = kpdrive::config::load();
        config.sync_photos = on;
        if let Err(e) = kpdrive::config::save(&config) {
            self.as_mut().set_status(QString::from(&format!("cannot save the setting: {e:#}")));
            return;
        }
        self.as_mut().set_sync_photos(on);
        // The daemon reads the setting each pass, so it needs no restart, but
        // a nudge starts the first download now rather than at the next poll.
        kpdrive::daemon::poke();
        let told = match on {
            true => "Proton Photos will be downloaded with the next sync",
            false => "Proton Photos will be left alone",
        };
        self.as_mut().set_status(QString::from(told));
    }

    pub fn change_log_level(mut self: Pin<&mut Self>, level: &QString) {
        let level = level.to_string();
        let Some(parsed) = kpdrive::config::LogLevel::parse(&level) else { return };
        // Load and amend: building a fresh Config would drop the other settings.
        let mut config = kpdrive::config::load();
        config.log_level = parsed;
        if let Err(e) = kpdrive::config::save(&config) {
            self.as_mut().set_status(QString::from(&format!("cannot save the setting: {e:#}")));
            return;
        }
        self.as_mut().set_log_level(QString::from(parsed.name()));
        let told = match parsed {
            kpdrive::config::LogLevel::Info => "Storing everything from now on",
            kpdrive::config::LogLevel::Warn => "Storing warnings and errors from now on",
            kpdrive::config::LogLevel::Error => "Storing errors from now on",
        };
        self.as_mut().set_status(QString::from(told));
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
