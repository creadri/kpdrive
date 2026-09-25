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
        /// Every account, by username, in the order they were added.
        #[qproperty(QStringList, accounts)]
        /// The account the per-account fields below are about.
        #[qproperty(QString, current_account, cxx_name = "currentAccount")]
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
        #[qproperty(QString, ingest_folder, cxx_name = "ingestFolder")]
        #[qproperty(bool, ingest_perm_rm, cxx_name = "ingestPermRm")]
        #[qproperty(QString, sync_status, cxx_name = "syncStatus")]
        #[qproperty(bool, sync_busy, cxx_name = "syncBusy")]
        #[qproperty(bool, daemon_running, cxx_name = "daemonRunning")]
        #[qproperty(bool, sync_failed, cxx_name = "syncFailed")]
        #[qproperty(bool, sync_offline, cxx_name = "syncOffline")]
        /// Paused for any reason, as the daemon last said.
        #[qproperty(bool, sync_paused, cxx_name = "syncPaused")]
        /// Paused by hand, as the setting says.
        #[qproperty(bool, paused_by_hand, cxx_name = "pausedByHand")]
        #[qproperty(QStringList, networks)]
        #[qproperty(QStringList, pause_networks, cxx_name = "pauseNetworks")]
        #[qproperty(QStringList, active_networks, cxx_name = "activeNetworks")]
        /// Power sources that pause, by config name: ac, battery, low_battery.
        #[qproperty(QStringList, pause_power, cxx_name = "pausePower")]
        /// The power source in use, by the same names.
        #[qproperty(QString, power_now, cxx_name = "powerNow")]
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

        /// Upload photos from `folder` into Proton Photos; empty stops it.
        #[qinvokable]
        #[cxx_name = "changeIngestFolder"]
        fn change_ingest_folder(self: Pin<&mut Self>, folder: &QString);

        /// Delete uploaded photos outright rather than trashing them.
        #[qinvokable]
        #[cxx_name = "changeIngestPermRm"]
        fn change_ingest_perm_rm(self: Pin<&mut Self>, on: bool);

        /// Pause or resume syncing by hand.
        #[qinvokable]
        #[cxx_name = "changePaused"]
        fn change_paused(self: Pin<&mut Self>, paused: bool);

        /// Pause, or stop pausing, while connected to `network`.
        #[qinvokable]
        #[cxx_name = "changeNetworkPause"]
        fn change_network_pause(self: Pin<&mut Self>, network: &QString, on: bool);

        /// Re-read the network connections NetworkManager knows.
        #[qinvokable]
        #[cxx_name = "reloadNetworks"]
        fn reload_networks(self: Pin<&mut Self>);

        /// Pause, or stop pausing, while on the power source named `power`.
        #[qinvokable]
        #[cxx_name = "changePowerPause"]
        fn change_power_pause(self: Pin<&mut Self>, power: &QString, on: bool);

        /// Re-read which power source is in use.
        #[qinvokable]
        #[cxx_name = "reloadPower"]
        fn reload_power(self: Pin<&mut Self>);

        /// Download the photos timeline into `folder` from now on.
        #[qinvokable]
        #[cxx_name = "changePhotosFolder"]
        fn change_photos_folder(self: Pin<&mut Self>, folder: &QString);

        /// One translated string, for QML to put in a label. Named `i18n`
        /// rather than `tr`, which already exists on every QObject.
        #[qinvokable]
        fn i18n(self: Pin<&mut Self>, text: &QString) -> QString;

        /// The singular or plural form for `n`, chosen by the catalog's rules.
        #[qinvokable]
        fn i18np(self: Pin<&mut Self>, singular: &QString, plural: &QString, n: i32) -> QString;

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

        /// Sign in through the browser: adds an account, or signs one that
        /// is already here back in.
        #[qinvokable]
        fn login(self: Pin<&mut Self>);

        /// Sign the selected account out. It keeps its folder.
        #[qinvokable]
        fn logout(self: Pin<&mut Self>);

        /// Forget the selected account and what it synced; its files stay.
        #[qinvokable]
        #[cxx_name = "removeAccount"]
        fn remove_account(self: Pin<&mut Self>);

        /// Show `username`'s account, folders and settings.
        #[qinvokable]
        #[cxx_name = "selectAccount"]
        fn select_account(self: Pin<&mut Self>, username: &QString);

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

/// What the window asks the worker to do, and for which account.
enum Task {
    /// Move a single-account setup over, once, before anything else.
    Migrate,
    Refresh(String),
    Login,
    Logout(String),
    Remove(String),
}

pub struct BackendRust {
    accounts: QStringList,
    current_account: QString,
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
    ingest_folder: QString,
    ingest_perm_rm: bool,
    sync_status: QString,
    sync_busy: bool,
    daemon_running: bool,
    sync_failed: bool,
    sync_offline: bool,
    sync_paused: bool,
    paused_by_hand: bool,
    networks: QStringList,
    pause_networks: QStringList,
    active_networks: QStringList,
    pause_power: QStringList,
    power_now: QString,
    sync_folder: QString,
    ignore_file: QString,
    version: QString,
    license: QString,
    tasks: Option<Sender<Task>>,
}

impl Default for BackendRust {
    fn default() -> Self {
        Self {
            accounts: QStringList::default(),
            current_account: QString::default(),
            username: QString::default(),
            used_bytes: 0.0,
            total_bytes: 0.0,
            logged_in: false,
            busy: false,
            status: QString::from(kpdrive::i18n::t("Starting…")),
            log_lines: QStringList::default(),
            retention_days: 30,
            log_level: QString::from("WARN"),
            sync_photos: false,
            photos_folder: QString::default(),
            ingest_folder: QString::default(),
            ingest_perm_rm: false,
            sync_status: QString::default(),
            sync_busy: false,
            daemon_running: false,
            sync_failed: false,
            sync_offline: false,
            sync_paused: false,
            paused_by_hand: false,
            networks: QStringList::default(),
            pause_networks: QStringList::default(),
            active_networks: QStringList::default(),
            pause_power: QStringList::default(),
            power_now: QString::default(),
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
                    Task::Migrate => {
                        if let Err(e) = runtime.block_on(kpdrive::account::migrate()) {
                            kpdrive::log::write("ERROR", &format!("could not move the account to the new layout: {e:#}"));
                        }
                        let _ = qt.queue(|b| b.reload_accounts(None));
                    }
                    Task::Refresh(username) => load_account(&runtime, &qt, &username),
                    Task::Login => {
                        let signal = qt.clone();
                        let result = runtime.block_on(kpdrive::account::login(|url, code| {
                            let message = kpdrive::i18n::fill(kpdrive::i18n::t("Confirm the code {code} in your browser"), &[("code", code)]);
                            let url = url.to_owned();
                            let _ = signal.queue(move |mut b| {
                                b.as_mut().set_status(QString::from(&message));
                                b.as_mut().open_url_requested(QString::from(&url));
                            });
                        }));
                        match result {
                            Ok((account, _)) => {
                                // The daemon starts on a new account, or takes
                                // up the session this sign-in replaced.
                                kpdrive::daemon::poke();
                                let _ = qt.queue(move |b| b.reload_accounts(Some(account.username)));
                            }
                            Err(e) => report(&qt, kpdrive::i18n::fill(kpdrive::i18n::t("Sign-in failed: {reason}"), &[("reason", &format!("{e:#}"))])),
                        }
                    }
                    Task::Logout(username) => {
                        let account = kpdrive::account::Account { username };
                        match runtime.block_on(account.logout()) {
                            Ok(()) => {
                                kpdrive::daemon::poke();
                                let _ = qt.queue(|mut b| {
                                    b.as_mut().set_logged_in(false);
                                    b.as_mut().set_used_bytes(0.0);
                                    b.as_mut().set_total_bytes(0.0);
                                    b.as_mut().set_busy(false);
                                    b.as_mut().set_status(QString::from(kpdrive::i18n::t("Signed out")));
                                });
                            }
                            Err(e) => report(&qt, kpdrive::i18n::fill(kpdrive::i18n::t("Sign-out failed: {reason}"), &[("reason", &format!("{e:#}"))])),
                        }
                    }
                    Task::Remove(username) => {
                        let account = kpdrive::account::Account { username };
                        match runtime.block_on(account.remove()) {
                            Ok(()) => {
                                kpdrive::daemon::poke();
                                let told = kpdrive::i18n::fill(kpdrive::i18n::t("Removed {account}. Its files were left where they are."), &[("account", &account.username)]);
                                let _ = qt.queue(move |mut b| {
                                    b.as_mut().set_busy(false);
                                    b.as_mut().reload_accounts(None);
                                    b.as_mut().set_status(QString::from(&told));
                                });
                            }
                            Err(e) => report(&qt, kpdrive::i18n::fill(kpdrive::i18n::t("Cannot remove the account: {reason}"), &[("reason", &format!("{e:#}"))])),
                        }
                    }
                }
            }
        });

        let config = kpdrive::config::load();
        self.as_mut().set_retention_days(config.log_retention_days as i32);
        self.as_mut().set_log_level(QString::from(config.log_level.name()));
        self.as_mut().set_paused_by_hand(config.sync_paused);
        self.as_mut().reload_networks();
        self.as_mut().reload_power();
        self.as_mut().reload_logs("");
        // The accounts are shown once the migration has had its turn.
        self.as_mut().set_busy(true);
        self.send(Task::Migrate);
    }
}

impl qobject::Backend {
    /// The selected account, if there is one.
    fn account(&self) -> Option<kpdrive::account::Account> {
        let name = self.current_account().to_string();
        kpdrive::account::all().into_iter().find(|a| a.username == name)
    }

    /// Re-reads the accounts and shows `select`, or the one already shown, or
    /// the first.
    fn reload_accounts(mut self: Pin<&mut Self>, select: Option<String>) {
        let accounts = kpdrive::account::all();
        let mut list = QStringList::default();
        for a in &accounts {
            list.append(QString::from(&a.username));
        }
        self.as_mut().set_accounts(list);
        let shown = self.current_account().to_string();
        let pick = select
            .and_then(|s| accounts.iter().find(|a| a.username.eq_ignore_ascii_case(&s)))
            .or_else(|| accounts.iter().find(|a| a.username == shown))
            .or(accounts.first())
            .map(|a| a.username.clone())
            .unwrap_or_default();
        self.as_mut().show_account(pick);
    }

    pub fn select_account(self: Pin<&mut Self>, username: &QString) {
        self.show_account(username.to_string());
    }

    /// Fills every per-account field for `username`, then asks Proton for
    /// its details.
    fn show_account(mut self: Pin<&mut Self>, username: String) {
        self.as_mut().set_current_account(QString::from(&username));
        self.as_mut().set_username(QString::from(&username));
        self.as_mut().set_used_bytes(0.0);
        self.as_mut().set_total_bytes(0.0);
        let account = self.account();
        let settings = account.as_ref().map(|a| a.settings()).unwrap_or_default();
        self.as_mut().set_sync_photos(settings.photos_sync);
        let photos = account.as_ref().and_then(|a| a.photos_folder().ok()).map(|d| d.display().to_string()).unwrap_or_default();
        self.as_mut().set_photos_folder(QString::from(&photos));
        let ingest = account.as_ref().and_then(|a| a.ingest_folder()).map(|d| d.display().to_string()).unwrap_or_default();
        self.as_mut().set_ingest_folder(QString::from(&ingest));
        self.as_mut().set_ingest_perm_rm(settings.photos_ingestion_perm_rm);
        self.as_mut().show_folder();
        self.as_mut().refresh_sync_status();
        match account {
            Some(_) => self.refresh(),
            None => {
                self.as_mut().set_logged_in(false);
                self.as_mut().set_busy(false);
            }
        }
    }

    /// Reads the configured folder into the two path properties.
    fn show_folder(mut self: Pin<&mut Self>) {
        let root = self.account().and_then(|a| current_root(&a));
        let folder = root.as_ref().map(|r| r.display().to_string()).unwrap_or_default();
        let ignore = root.map(|r| r.join(kpdrive::sync::IGNORE_FILE).display().to_string()).unwrap_or_default();
        self.as_mut().set_sync_folder(QString::from(&folder));
        self.as_mut().set_ignore_file(QString::from(&ignore));
    }

    /// The question to put before syncing into `folder`, empty when there is
    /// nothing to ask. The wording comes from the same place the CLI reads it.
    pub fn folder_question(self: Pin<&mut Self>, folder: &QString) -> QString {
        let folder = std::path::PathBuf::from(folder.to_string());
        let state = self.account().and_then(|a| kpdrive::sync::load_state(&a).ok().flatten()).unwrap_or_default();
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
        let Some(account) = self.account() else { return };
        let occupied = match choice.to_string().as_str() {
            "rename" => kpdrive::sync::Occupied::Rename,
            _ => kpdrive::sync::Occupied::Merge,
        };
        let changed = kpdrive::sync::open_state(&account).and_then(|mut state| kpdrive::sync::set_folder(&account, &mut state, folder, occupied));
        match changed {
            Ok(note) => {
                // The daemon notices the new folder when poked, and starts
                // that account again from there.
                kpdrive::daemon::poke();
                self.as_mut().set_status(QString::from(&note));
            }
            Err(e) => self.as_mut().set_status(QString::from(&kpdrive::i18n::fill(kpdrive::i18n::t("Cannot change the folder: {reason}"), &[("reason", &format!("{e:#}"))]))),
        }
        self.show_folder();
    }

    pub fn open_ignore_file(mut self: Pin<&mut Self>) {
        let Some(root) = self.account().and_then(|a| current_root(&a)) else {
            self.as_mut().set_status(QString::from(kpdrive::i18n::t("No sync folder yet. Run: kpdrive setup")));
            return;
        };
        if let Err(e) = kpdrive::setup::ignore_template(&root) {
            self.as_mut().set_status(QString::from(&kpdrive::i18n::fill(kpdrive::i18n::t("Cannot create the ignore file: {reason}"), &[("reason", &format!("{e:#}"))])));
            return;
        }
        let url = format!("file://{}", root.join(kpdrive::sync::IGNORE_FILE).display());
        self.as_mut().open_url_requested(QString::from(&url));
    }

    /// QML asks for its strings here, because the catalog lives in the library
    /// with the wording the terminal shares. The runtime looks strings up by
    /// content, so the literal in the QML is both the key and the fallback.
    pub fn i18n(self: Pin<&mut Self>, text: &QString) -> QString {
        QString::from(&kpdrive::i18n::lookup(&text.to_string()))
    }

    pub fn i18np(self: Pin<&mut Self>, singular: &QString, plural: &QString, n: i32) -> QString {
        QString::from(&kpdrive::i18n::lookup_plural(&singular.to_string(), &plural.to_string(), n.max(0) as u64))
    }

    /// The daemon's own account of itself, in the words `kpdrive status` uses.
    pub fn refresh_sync_status(mut self: Pin<&mut Self>) {
        let report = kpdrive::daemon::ask();
        let name = self.current_account().to_string();
        let mine = report.account(&name).cloned().unwrap_or_default();
        self.as_mut().set_daemon_running(report.running);
        self.as_mut().set_sync_busy(mine.syncing);
        // An outage clears itself, so it reads as something to wait out
        // rather than something that went wrong.
        self.as_mut().set_sync_offline(mine.offline);
        self.as_mut().set_sync_failed(mine.signed_out || (mine.error.is_some() && !mine.offline));
        let sentence = report.sentence(Some(&name).filter(|n| !n.is_empty()).map(String::as_str));
        self.as_mut().set_sync_status(QString::from(&sentence));
        self.as_mut().set_sync_paused(report.paused.is_some());
    }

    pub fn sync_now(self: Pin<&mut Self>) {
        match self.account() {
            Some(a) => kpdrive::daemon::poke_account(&a.username),
            None => kpdrive::daemon::poke(),
        }
        self.refresh_sync_status();
    }

    pub fn refresh(mut self: Pin<&mut Self>) {
        let Some(account) = self.account() else { return };
        self.as_mut().set_busy(true);
        self.as_mut().set_status(QString::from(kpdrive::i18n::t("Checking the account…")));
        self.send(Task::Refresh(account.username));
    }

    pub fn login(mut self: Pin<&mut Self>) {
        self.as_mut().set_busy(true);
        self.as_mut().set_status(QString::from(kpdrive::i18n::t("Opening the browser…")));
        self.send(Task::Login);
    }

    pub fn logout(mut self: Pin<&mut Self>) {
        let Some(account) = self.account() else { return };
        self.as_mut().set_busy(true);
        self.as_mut().set_status(QString::from(kpdrive::i18n::t("Signing out…")));
        self.send(Task::Logout(account.username));
    }

    pub fn remove_account(mut self: Pin<&mut Self>) {
        let Some(account) = self.account() else { return };
        self.as_mut().set_busy(true);
        self.as_mut().set_status(QString::from(kpdrive::i18n::t("Removing the account…")));
        self.send(Task::Remove(account.username));
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
            Err(e) => list.append(QString::from(&kpdrive::i18n::fill(kpdrive::i18n::t("Cannot read the log: {reason}"), &[("reason", &format!("{e:#}"))]))),
        }
        self.as_mut().set_log_lines(list);
    }

    pub fn change_sync_photos(mut self: Pin<&mut Self>, on: bool) {
        let Some(account) = self.account() else { return };
        if let Err(e) = account.update(|a| a.photos_sync = on) {
            self.as_mut().set_status(QString::from(&kpdrive::i18n::fill(kpdrive::i18n::t("Cannot save the setting: {reason}"), &[("reason", &format!("{e:#}"))])));
            return;
        }
        self.as_mut().set_sync_photos(on);
        // The daemon reads the setting each pass, so it needs no restart, but
        // a nudge starts the first download now rather than at the next poll.
        kpdrive::daemon::poke();
        let told = match on {
            true => kpdrive::i18n::t("Proton Photos will be downloaded with the next sync"),
            false => kpdrive::i18n::t("Proton Photos will be left alone"),
        };
        self.as_mut().set_status(QString::from(told));
    }

    pub fn change_ingest_folder(mut self: Pin<&mut Self>, folder: &QString) {
        let Some(account) = self.account() else { return };
        let folder = std::path::PathBuf::from(folder.to_string());
        if !folder.as_os_str().is_empty() {
            let (root, dest) = (current_root(&account), account.photos_folder().ok());
            let others: Vec<&std::path::Path> = root.as_deref().into_iter().chain(dest.as_deref()).collect();
            if let Err(e) = kpdrive::ingest::check_folder(&folder, &others).and_then(|_| account.check_folder(&folder)) {
                self.as_mut().set_status(QString::from(&format!("{e:#}")));
                return;
            }
        }
        let chosen = Some(folder.clone()).filter(|f| !f.as_os_str().is_empty());
        if let Err(e) = account.update(|a| a.photos_ingestion_folder = chosen.clone()) {
            self.as_mut().set_status(QString::from(&kpdrive::i18n::fill(kpdrive::i18n::t("Cannot save the setting: {reason}"), &[("reason", &format!("{e:#}"))])));
            return;
        }
        self.as_mut().set_ingest_folder(QString::from(&folder.display().to_string()));
        kpdrive::daemon::poke();
        let told = match chosen {
            Some(_) => kpdrive::i18n::t("Photos put in that folder will be uploaded to Proton Photos"),
            None => kpdrive::i18n::t("No longer uploading photos"),
        };
        self.as_mut().set_status(QString::from(told));
    }

    pub fn change_paused(mut self: Pin<&mut Self>, paused: bool) {
        if let Err(e) = kpdrive::pause::set(paused) {
            self.as_mut().set_status(QString::from(&kpdrive::i18n::fill(kpdrive::i18n::t("Cannot save the setting: {reason}"), &[("reason", &format!("{e:#}"))])));
            return;
        }
        self.as_mut().set_paused_by_hand(paused);
        self.refresh_sync_status();
    }

    pub fn change_network_pause(mut self: Pin<&mut Self>, network: &QString, on: bool) {
        let network = network.to_string();
        let mut config = kpdrive::config::load();
        config.pause_on_networks.retain(|n| n != &network);
        if on {
            config.pause_on_networks.push(network);
        }
        if let Err(e) = kpdrive::config::save(&config) {
            self.as_mut().set_status(QString::from(&kpdrive::i18n::fill(kpdrive::i18n::t("Cannot save the setting: {reason}"), &[("reason", &format!("{e:#}"))])));
            return;
        }
        // Takes effect at once when that network is the one in use.
        kpdrive::daemon::poke();
        self.as_mut().reload_networks();
    }

    /// Every connection NetworkManager knows, plus any chosen one it has
    /// since forgotten, so that one can still be unticked.
    pub fn reload_networks(mut self: Pin<&mut Self>) {
        let chosen = kpdrive::config::load().pause_on_networks;
        let mut known = kpdrive::pause::known_connections();
        known.extend(chosen.iter().filter(|c| !known.contains(c)).cloned().collect::<Vec<_>>());
        let list = |names: &[String]| {
            let mut l = QStringList::default();
            for n in names {
                l.append(QString::from(n));
            }
            l
        };
        self.as_mut().set_networks(list(&known));
        self.as_mut().set_pause_networks(list(&chosen));
        self.as_mut().set_active_networks(list(&kpdrive::pause::active_connections()));
    }

    pub fn change_power_pause(mut self: Pin<&mut Self>, power: &QString, on: bool) {
        let Some(power) = kpdrive::config::Power::parse(&power.to_string()) else { return };
        let mut config = kpdrive::config::load();
        config.pause_on_power.retain(|p| *p != power);
        if on {
            config.pause_on_power.push(power);
        }
        if let Err(e) = kpdrive::config::save(&config) {
            self.as_mut().set_status(QString::from(&kpdrive::i18n::fill(kpdrive::i18n::t("Cannot save the setting: {reason}"), &[("reason", &format!("{e:#}"))])));
            return;
        }
        kpdrive::daemon::poke();
        self.as_mut().reload_power();
    }

    pub fn reload_power(mut self: Pin<&mut Self>) {
        let mut chosen = QStringList::default();
        for p in kpdrive::config::load().pause_on_power {
            chosen.append(QString::from(p.name()));
        }
        self.as_mut().set_pause_power(chosen);
        self.as_mut().set_power_now(QString::from(kpdrive::pause::power().name()));
    }

    pub fn change_photos_folder(mut self: Pin<&mut Self>, folder: &QString) {
        let Some(account) = self.account() else { return };
        let folder = std::path::PathBuf::from(folder.to_string());
        if folder.as_os_str().is_empty() {
            return;
        }
        // The same guards as the CLI, plus the ingestion folder: photos
        // downloaded into it would be uploaded straight back.
        let checked = kpdrive::photos::check_dest(&folder, current_root(&account).as_deref())
            .and_then(|_| match account.ingest_folder() {
                Some(ingest) => kpdrive::ingest::check_folder(&ingest, &[&folder]),
                None => Ok(()),
            })
            .and_then(|_| account.check_folder(&folder))
            .and_then(|_| kpdrive::photos::set_dest(&account, folder.clone()));
        if let Err(e) = checked {
            self.as_mut().set_status(QString::from(&format!("{e:#}")));
            return;
        }
        self.as_mut().set_photos_folder(QString::from(&folder.display().to_string()));
        self.as_mut().set_status(QString::from(kpdrive::i18n::t("Photos will be downloaded into that folder from the next pass")));
    }

    pub fn change_ingest_perm_rm(mut self: Pin<&mut Self>, on: bool) {
        let Some(account) = self.account() else { return };
        if let Err(e) = account.update(|a| a.photos_ingestion_perm_rm = on) {
            self.as_mut().set_status(QString::from(&kpdrive::i18n::fill(kpdrive::i18n::t("Cannot save the setting: {reason}"), &[("reason", &format!("{e:#}"))])));
            return;
        }
        self.as_mut().set_ingest_perm_rm(on);
    }

    pub fn change_log_level(mut self: Pin<&mut Self>, level: &QString) {
        let level = level.to_string();
        let Some(parsed) = kpdrive::config::LogLevel::parse(&level) else { return };
        // Load and amend: building a fresh Config would drop the other settings.
        let mut config = kpdrive::config::load();
        config.log_level = parsed;
        if let Err(e) = kpdrive::config::save(&config) {
            self.as_mut().set_status(QString::from(&kpdrive::i18n::fill(kpdrive::i18n::t("Cannot save the setting: {reason}"), &[("reason", &format!("{e:#}"))])));
            return;
        }
        self.as_mut().set_log_level(QString::from(parsed.name()));
        let told = match parsed {
            kpdrive::config::LogLevel::Info => kpdrive::i18n::t("Storing everything from now on"),
            kpdrive::config::LogLevel::Warn => kpdrive::i18n::t("Storing warnings and errors from now on"),
            kpdrive::config::LogLevel::Error => kpdrive::i18n::t("Storing errors from now on"),
        };
        self.as_mut().set_status(QString::from(told));
    }

    pub fn set_retention(mut self: Pin<&mut Self>, days: i32) {
        let days = days.clamp(1, 3650) as u64;
        // Load and amend: building a fresh Config would drop the sync folder.
        let mut config = kpdrive::config::load();
        config.log_retention_days = days;
        if let Err(e) = kpdrive::config::save(&config) {
            self.as_mut().set_status(QString::from(&kpdrive::i18n::fill(kpdrive::i18n::t("Cannot save the setting: {reason}"), &[("reason", &format!("{e:#}"))])));
            return;
        }
        match kpdrive::log::prune(days) {
            Ok(0) => {}
            Ok(n) => self.as_mut().set_status(QString::from(&kpdrive::i18n::fill(kpdrive::i18n::tn("removed {n} log file older than {days} days", "removed {n} log files older than {days} days", n as u64), &[("n", &n.to_string()), ("days", &days.to_string())]))),
            Err(e) => self.as_mut().set_status(QString::from(&kpdrive::i18n::fill(kpdrive::i18n::t("Cannot prune the log: {reason}"), &[("reason", &format!("{e:#}"))]))),
        }
        self.as_mut().set_retention_days(days as i32);
        self.reload_logs("");
    }
}

/// The account's sync folder, falling back to what its sync state recorded.
fn current_root(account: &kpdrive::account::Account) -> Option<std::path::PathBuf> {
    account
        .sync_folder()
        .or_else(|| kpdrive::sync::load_state(account).ok().flatten().map(|s| s.root))
        .filter(|p| !p.as_os_str().is_empty())
}

/// Loads `username`'s details and pushes them into the window, unless it
/// shows another account by the time they arrive.
fn load_account(runtime: &tokio::runtime::Runtime, qt: &cxx_qt::CxxQtThread<qobject::Backend>, username: &str) {
    let account = kpdrive::account::Account { username: username.to_owned() };
    let asked = username.to_owned();
    let still = move |b: &Pin<&mut qobject::Backend>| b.current_account().to_string() == asked;
    match runtime.block_on(account.info()) {
        Ok(info) => {
            let _ = qt.queue(move |mut b| {
                if !still(&b) {
                    return;
                }
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
                if !still(&b) {
                    return;
                }
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
