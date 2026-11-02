// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The update check service (ADR-0066): one task on the app's tokio runtime.
//!
//! [`AppUpdates`] implements [`UpdateService`]. Its methods only send a message to the task and never block. The
//! task owns the [`CheckSchedule`], the persisted `update-state.toml` and the in-flight check, and it talks to the UI
//! through [`UpdateEvent`]s. It never calls `check()` while the setting is off or before the window asked it to
//! start (the first frame), so nothing leaves the machine until the user has opted in.
//!
//! The HTTP client (it loads the platform root certificates) is built lazily, on the first check, off the UI path.
//!
//! ADR-0078: after a check finds a newer version the task also downloads and verifies the artifact for this
//! install (archive or `SQLer.app` disk image) in the background, and installs it only when the user asks
//! ([`Control::Install`]). Install and relaunch run on blocking threads; the UI only sees [`DownloadEvent`]s.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sqler_config::{ConfigDir, UpdateState};
use sqler_ui::update::{DownloadEvent, UpdateEvent, UpdateOutcome, UpdateService, UpdateStartup};
use sqler_update::{
    CheckOutcome, CheckSchedule, DEFAULT_FEED_URL, InstallHooks, InstallOutcome, UPDATES_DIR_NAME,
    UpdateClient, UpdateError, UpdateOffer, VerifiedArtifact, clean_leftovers,
    default_package_dirs, embedded_keys, plan_install,
};
use tokio::runtime::Handle;
use tokio::sync::OnceCell;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::task::{JoinError, JoinHandle};
use tokio::time::Instant;

/// The shortest wait when a timer fires before a check is due.
const MIN_REARM: Duration = Duration::from_secs(1);

type ClientFactory = Arc<dyn Fn() -> Result<UpdateClient, UpdateError> + Send + Sync>;
type CheckResult = Result<CheckOutcome, UpdateError>;
type Probe = Arc<dyn Fn(&Path) -> Option<String> + Send + Sync>;
type ExtractApp = Arc<dyn Fn(&Path, &Path) -> Result<(), UpdateError> + Send + Sync>;
type BundleCheck = Arc<dyn Fn(&Path) -> bool + Send + Sync>;
type Relaunch = Arc<dyn Fn(&Path) -> io::Result<()> + Send + Sync>;

/// The shortest gap between two progress reports.
const PROGRESS_EVERY: Duration = Duration::from_millis(250);
/// How long the new program may take to print its version.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// What the service task needs; the seam for tests.
pub(crate) struct Deps {
    /// Builds the HTTP client. Called on a blocking thread, at most once per successful build.
    pub make_client: ClientFactory,
    /// Whether a trusted key is compiled in.
    pub configured: bool,
    /// When to check.
    pub schedule: CheckSchedule,
    /// Where `update-state.toml` lives; `None` keeps the state in memory only.
    pub dir: Option<ConfigDir>,
    /// The wall clock.
    pub clock: fn() -> SystemTime,
    /// The running program (canonical); `None` turns the download off.
    pub exe: Option<PathBuf>,
    /// `<config>/updates`; `None` turns the download off.
    pub updates_dir: Option<PathBuf>,
    /// Folders owned by a package manager or an installer.
    pub package_dirs: Vec<PathBuf>,
    /// Runs the new program with `--version`.
    pub version_probe: Probe,
    /// Copies `SQLer.app` out of a disk image `(dmg, dest)`.
    pub extract_app: ExtractApp,
    /// Checks the signature of a new `.app`.
    pub bundle_check: BundleCheck,
    /// Starts the new version after this process exits.
    pub relaunch: Relaunch,
}

/// The production service: the real feed, the compiled-in keys, the default schedule.
pub(crate) fn startup(dir: Option<&ConfigDir>, rt: &Handle) -> UpdateStartup {
    let Ok(current) = semver::Version::parse(env!("CARGO_PKG_VERSION")) else {
        return UpdateStartup::unavailable();
    };
    let state = dir.map(ConfigDir::load_update_state).unwrap_or_default();
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.canonicalize().ok());
    let deps = Deps {
        make_client: Arc::new(move || UpdateClient::new(DEFAULT_FEED_URL, current.clone())),
        configured: !embedded_keys().is_empty(),
        schedule: CheckSchedule::default(),
        dir: dir.cloned(),
        clock: SystemTime::now,
        exe: exe.clone(),
        updates_dir: dir.map(|d| d.root().join(UPDATES_DIR_NAME)),
        package_dirs: default_package_dirs(),
        version_probe: Arc::new(probe_version),
        extract_app: Arc::new(extract_app),
        bundle_check: Arc::new(bundle_check),
        relaunch: Arc::new(relaunch),
    };
    if let Some(exe) = exe {
        // Leftovers of an earlier install (the stage folder and the replaced version), off the UI path.
        rt.spawn_blocking(move || clean_leftovers(&exe));
    }
    spawn(deps, state, rt)
}

#[cfg(target_os = "macos")]
fn extract_app(dmg: &Path, dest: &Path) -> Result<(), UpdateError> {
    sqler_update::hdiutil_extract_app(dmg, dest)
}

#[cfg(not(target_os = "macos"))]
fn extract_app(_dmg: &Path, _dest: &Path) -> Result<(), UpdateError> {
    Err(UpdateError::ArchiveLayout)
}

#[cfg(target_os = "macos")]
fn bundle_check(app: &Path) -> bool {
    sqler_update::codesign_check(app)
}

#[cfg(not(target_os = "macos"))]
fn bundle_check(_app: &Path) -> bool {
    false
}

/// Runs `<program> --version` with piped output and a kill timeout; `None` on any failure.
fn probe_version(program: &Path) -> Option<String> {
    use std::io::Read as _;
    use std::process::{Command, Stdio};
    let mut child = Command::new(program)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) if started.elapsed() > PROBE_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    let mut out = String::new();
    child
        .stdout
        .take()?
        .take(4096)
        .read_to_string(&mut out)
        .ok()?;
    Some(out)
}

/// Starts `launch` once this process has exited. Detached: it outlives us and has no console.
#[cfg(unix)]
fn relaunch(launch: &Path) -> io::Result<()> {
    use std::os::unix::process::CommandExt as _;
    use std::process::{Command, Stdio};
    const SCRIPT: &str = r#"while kill -0 "$0" 2>/dev/null; do sleep 0.2; done; case "$1" in *.app) exec /usr/bin/open -n "$1";; *) exec "$1";; esac"#;
    Command::new("/bin/sh")
        .arg("-c")
        .arg(SCRIPT)
        .arg(std::process::id().to_string())
        .arg(launch)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(drop)
}

/// Starts `launch` once this process has exited. Detached: it outlives us and has no console.
#[cfg(windows)]
fn relaunch(launch: &Path) -> io::Result<()> {
    use std::os::windows::process::CommandExt as _;
    use std::process::{Command, Stdio};
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const SCRIPT: &str = "Wait-Process -Id ([int]$env:SQLER_RELAUNCH_PID) -ErrorAction SilentlyContinue; \
        Start-Process -FilePath $env:SQLER_RELAUNCH_EXE";
    // By absolute path: a bare name is searched in the application directory first, which is user-writable here.
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    let powershell = Path::new(&root).join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    Command::new(powershell)
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-Command",
            SCRIPT,
        ])
        .env("SQLER_RELAUNCH_PID", std::process::id().to_string())
        .env("SQLER_RELAUNCH_EXE", launch)
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(drop)
}

/// Removes every entry of `updates/` except `keep` (`updates/<version>`): an older version's artifact or orphan `.part`.
fn clear_other_versions(keep: &Path) {
    let Some(parent) = keep.parent() else { return };
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path == keep {
            continue;
        }
        let _ = if entry.file_type().is_ok_and(|t| t.is_dir()) {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
    }
}

/// Starts the service task on `rt` and returns what the window takes.
pub(crate) fn spawn(deps: Deps, state: UpdateState, rt: &Handle) -> UpdateStartup {
    let (control_tx, control_rx) = unbounded_channel();
    let (events_tx, events_rx) = unbounded_channel();
    let configured = deps.configured;
    let dismissed = state.dismissed_version.clone();
    rt.spawn(run(deps, state, control_rx, events_tx));
    UpdateStartup {
        service: Some(Arc::new(AppUpdates {
            configured,
            control: control_tx,
        })),
        events: Some(events_rx),
        dismissed,
    }
}

enum Control {
    Start(bool),
    SetEnabled(bool),
    CheckNow,
    Dismiss(String),
    Install(String),
}

struct AppUpdates {
    configured: bool,
    control: UnboundedSender<Control>,
}

impl AppUpdates {
    fn send(&self, message: Control) {
        // The task ends only when the service is dropped; a send after that has nobody to tell.
        let _ = self.control.send(message);
    }
}

impl UpdateService for AppUpdates {
    fn configured(&self) -> bool {
        self.configured
    }

    fn start(&self, enabled: bool) {
        self.send(Control::Start(enabled));
    }

    fn set_enabled(&self, enabled: bool) {
        self.send(Control::SetEnabled(enabled));
    }

    fn check_now(&self) {
        self.send(Control::CheckNow);
    }

    fn dismiss(&self, version: &str) {
        self.send(Control::Dismiss(version.to_owned()));
    }

    fn install(&self, version: &str) {
        self.send(Control::Install(version.to_owned()));
    }
}

struct Inflight {
    handle: JoinHandle<CheckResult>,
    manual: bool,
}

/// The download of one version, running.
struct Download {
    version: String,
    handle: JoinHandle<Option<VerifiedArtifact>>,
}

struct Task {
    deps: Deps,
    state: UpdateState,
    events: UnboundedSender<UpdateEvent>,
    client: Arc<OnceCell<UpdateClient>>,
    enabled: bool,
    started: bool,
    deadline: Option<Instant>,
    inflight: Option<Inflight>,
    /// The download in progress.
    download: Option<Download>,
    /// The newest verified artifact, kept until it is replaced, refused or the checks are turned off.
    held: Option<VerifiedArtifact>,
    /// The version installed in this run. It is never offered or downloaded again: the running process still
    /// executes from the `previous` copy that a second install of it would delete.
    installed: Option<String>,
}

async fn run(
    deps: Deps,
    state: UpdateState,
    mut control: UnboundedReceiver<Control>,
    events: UnboundedSender<UpdateEvent>,
) {
    let mut task = Task {
        deps,
        state,
        events,
        client: Arc::new(OnceCell::new()),
        enabled: false,
        started: false,
        deadline: None,
        inflight: None,
        download: None,
        held: None,
        installed: None,
    };
    loop {
        tokio::select! {
            message = control.recv() => {
                let Some(message) = message else { break };
                task.handle(message).await;
            }
            () = sleep_until_opt(task.deadline), if task.deadline.is_some() => {
                task.on_timer();
            }
            (joined, manual) = wait(&mut task.inflight), if task.inflight.is_some() => {
                task.inflight = None;
                task.finished(joined, manual).await;
            }
            joined = wait_download(&mut task.download), if task.download.is_some() => {
                task.download = None;
                if let Ok(Some(artifact)) = joined {
                    task.held = Some(artifact);
                }
            }
        }
    }
    task.abort_inflight();
    if let Some(running) = task.download.take() {
        running.handle.abort();
    }
}

async fn wait_download(
    download: &mut Option<Download>,
) -> Result<Option<VerifiedArtifact>, JoinError> {
    match download {
        Some(running) => (&mut running.handle).await,
        None => std::future::pending().await,
    }
}

async fn sleep_until_opt(deadline: Option<Instant>) {
    match deadline {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

async fn wait(inflight: &mut Option<Inflight>) -> (Result<CheckResult, JoinError>, bool) {
    match inflight {
        Some(running) => {
            let joined = (&mut running.handle).await;
            (joined, running.manual)
        }
        None => std::future::pending().await,
    }
}

fn unix_secs(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

impl Task {
    async fn handle(&mut self, message: Control) {
        match message {
            Control::Start(on) => {
                self.started = true;
                self.set_enabled(on, true).await;
            }
            Control::SetEnabled(on) => self.set_enabled(on, self.started).await,
            Control::Install(version) => self.install(&version).await,
            Control::CheckNow => self.begin(true),
            Control::Dismiss(version) => {
                self.state.dismissed_version = Some(version);
                self.persist().await;
            }
        }
    }

    /// On arms the timer (once, `arm` false keeps it idle until `Start`); off stops the timer and aborts the check.
    /// Off also aborts the download and removes the downloaded files.
    async fn set_enabled(&mut self, on: bool, arm: bool) {
        if on {
            self.enabled = true;
            if arm && self.deadline.is_none() && self.inflight.is_none() {
                self.deadline = Some(Instant::now() + self.deps.schedule.startup_delay);
            }
        } else {
            self.enabled = false;
            self.deadline = None;
            self.abort_inflight();
            self.drop_downloads().await;
        }
    }

    /// Aborts the download, forgets the held artifact and deletes `updates/` on a blocking thread.
    async fn drop_downloads(&mut self) {
        self.held = None;
        if let Some(running) = self.download.take() {
            running.handle.abort();
            let _ = running.handle.await;
        }
        if let Some(dir) = self.deps.updates_dir.clone() {
            let _ = tokio::task::spawn_blocking(move || std::fs::remove_dir_all(dir)).await;
        }
    }

    fn abort_inflight(&mut self) {
        if let Some(running) = self.inflight.take() {
            running.handle.abort();
        }
    }

    fn send(&self, event: UpdateEvent) {
        let _ = self.events.send(event);
    }

    fn last_attempt(&self) -> Option<SystemTime> {
        self.state
            .last_attempt
            .map(|s| UNIX_EPOCH + Duration::from_secs(s))
    }

    fn on_timer(&mut self) {
        self.deadline = None;
        if !self.enabled || !self.started || self.inflight.is_some() {
            return;
        }
        let now = (self.deps.clock)();
        let failures = self.state.consecutive_failures;
        if self
            .deps
            .schedule
            .is_due(now, self.last_attempt(), failures)
        {
            self.begin(false);
            return;
        }
        let wait = self.deps.schedule.delay_after(failures);
        let elapsed = self
            .last_attempt()
            .and_then(|last| now.duration_since(last).ok())
            .unwrap_or_default();
        self.deadline = Some(Instant::now() + wait.saturating_sub(elapsed).max(MIN_REARM));
    }

    /// Starts a check. Does nothing while off, before `Start` or while one runs.
    fn begin(&mut self, manual: bool) {
        if !self.enabled || !self.started || self.inflight.is_some() {
            return;
        }
        if !self.deps.configured {
            self.send(UpdateEvent::Finished {
                manual,
                outcome: UpdateOutcome::NotConfigured,
            });
            return;
        }
        self.send(UpdateEvent::Started { manual });
        let make = Arc::clone(&self.deps.make_client);
        let cell = Arc::clone(&self.client);
        let handle = tokio::spawn(async move {
            let client = cell
                .get_or_try_init(|| async move {
                    match tokio::task::spawn_blocking(move || make()).await {
                        Ok(built) => built,
                        Err(_) => Err(UpdateError::Network),
                    }
                })
                .await?;
            client.check().await
        });
        self.inflight = Some(Inflight { handle, manual });
    }

    async fn finished(&mut self, joined: Result<CheckResult, JoinError>, manual: bool) {
        let result = joined.unwrap_or(Err(UpdateError::Network));
        if matches!(result, Err(UpdateError::NotConfigured)) {
            self.send(UpdateEvent::Finished {
                manual,
                outcome: UpdateOutcome::NotConfigured,
            });
            return;
        }
        self.state.last_attempt = Some(unix_secs((self.deps.clock)()));
        self.state.consecutive_failures = match result {
            Ok(_) => 0,
            Err(_) => self.state.consecutive_failures.saturating_add(1),
        };
        self.persist().await;
        let mut offered = None;
        let outcome = match result {
            Ok(CheckOutcome::UpToDate { latest }) => UpdateOutcome::UpToDate {
                latest: latest.to_string(),
            },
            Ok(CheckOutcome::Available(offer)) => {
                let outcome = UpdateOutcome::Available {
                    version: offer.version.to_string(),
                    notes_url: offer.notes_url.clone(),
                };
                offered = Some(offer);
                outcome
            }
            Err(err) => UpdateOutcome::Failed {
                message: err.to_string(),
            },
        };
        self.send(UpdateEvent::Finished { manual, outcome });
        if let Some(offer) = offered {
            self.begin_download(offer, manual);
        }
        if self.enabled {
            let wait = self
                .deps
                .schedule
                .delay_after(self.state.consecutive_failures);
            self.deadline = Some(Instant::now() + wait);
        }
    }

    /// Starts the download for a newer version. A failed re-check never reaches here, so a held artifact survives it.
    fn begin_download(&mut self, offer: UpdateOffer, manual: bool) {
        let version = offer.version.to_string();
        let automatic_and_dismissed =
            !manual && self.state.dismissed_version.as_deref() == Some(version.as_str());
        let (Some(exe), Some(updates)) = (self.deps.exe.clone(), self.deps.updates_dir.clone())
        else {
            return;
        };
        if !self.enabled
            || automatic_and_dismissed
            || self.installed.as_deref() == Some(version.as_str())
        {
            return;
        }
        if self
            .held
            .as_ref()
            .is_some_and(|h| h.version == offer.version)
        {
            self.send(UpdateEvent::Download(DownloadEvent::Verified { version }));
            return;
        }
        if self.download.as_ref().is_some_and(|d| d.version == version) {
            return;
        }
        if let Some(old) = self.download.take() {
            old.handle.abort();
        }
        self.held = None;
        let cell = Arc::clone(&self.client);
        let kind = plan_install(&exe, &self.deps.package_dirs).kind;
        let events = self.events.clone();
        let dir = updates.join(&version);
        let announced = version.clone();
        let handle = tokio::spawn(async move {
            let last = Mutex::new(None::<std::time::Instant>);
            let tx = events.clone();
            let ver = announced.clone();
            let progress = move |received: u64, total: u64| {
                let Ok(mut last) = last.lock() else { return };
                let now = std::time::Instant::now();
                if last.is_some_and(|at| now.duration_since(at) < PROGRESS_EVERY) {
                    return;
                }
                *last = Some(now);
                let _ = tx.send(UpdateEvent::Download(DownloadEvent::Progress {
                    version: ver.clone(),
                    received,
                    total,
                }));
            };
            let keep = dir.clone();
            let _ = tokio::task::spawn_blocking(move || clear_other_versions(&keep)).await;
            let client = cell.get()?;
            match client.download(&offer, kind, &dir, &progress).await {
                Ok(artifact) => {
                    let _ = events.send(UpdateEvent::Download(DownloadEvent::Verified {
                        version: announced,
                    }));
                    Some(artifact)
                }
                Err(err) => {
                    let _ = events.send(UpdateEvent::Download(DownloadEvent::Refused {
                        version: announced,
                        reason: err.to_string(),
                    }));
                    None
                }
            }
        });
        self.download = Some(Download { version, handle });
    }

    /// Installs the held artifact on a blocking thread, then starts the relauncher.
    async fn install(&mut self, version: &str) {
        let failed = |reason: &str| {
            UpdateEvent::Download(DownloadEvent::InstallFailed {
                version: version.to_owned(),
                reason: reason.to_owned(),
            })
        };
        let held = self
            .held
            .clone()
            .filter(|h| h.version.to_string() == version);
        let (Some(artifact), Some(exe)) = (held, self.deps.exe.clone()) else {
            self.send(failed("No verified update is ready to install."));
            return;
        };
        self.send(UpdateEvent::Download(DownloadEvent::Installing {
            version: version.to_owned(),
        }));
        let packages = self.deps.package_dirs.clone();
        let (probe, extract, check) = (
            Arc::clone(&self.deps.version_probe),
            Arc::clone(&self.deps.extract_app),
            Arc::clone(&self.deps.bundle_check),
        );
        let file = artifact.clone();
        let joined = tokio::task::spawn_blocking(move || {
            let hooks = InstallHooks {
                version_probe: &*probe,
                extract_app: &*extract,
                bundle_check: &*check,
            };
            sqler_update::install(&file, &exe, &packages, &hooks)
        })
        .await;
        match joined {
            Ok(Ok(InstallOutcome::Installed { launch })) => {
                let relaunch = Arc::clone(&self.deps.relaunch);
                let ok = tokio::task::spawn_blocking(move || relaunch(&launch).is_ok())
                    .await
                    .unwrap_or(false);
                let _ = std::fs::remove_file(&artifact.path);
                self.held = None;
                self.installed = Some(version.to_owned());
                self.send(UpdateEvent::Download(DownloadEvent::Restarting {
                    version: version.to_owned(),
                    relaunch: ok,
                }));
            }
            Ok(Ok(InstallOutcome::Manual(reason))) => {
                self.send(UpdateEvent::Download(DownloadEvent::Manual {
                    version: version.to_owned(),
                    reason: reason.note().to_owned(),
                    path: artifact.path.display().to_string(),
                }));
            }
            Ok(Err(UpdateError::ChangedAfterVerification)) => {
                let reason = UpdateError::ChangedAfterVerification.to_string();
                let path = artifact.path.clone();
                self.held = None;
                let _ = tokio::task::spawn_blocking(move || std::fs::remove_file(path)).await;
                self.send(UpdateEvent::Download(DownloadEvent::Refused {
                    version: version.to_owned(),
                    reason,
                }));
            }
            Ok(Err(err)) => self.send(failed(&err.to_string())),
            Err(_) => self.send(failed("the update could not be installed")),
        }
    }

    /// Saves the state on a blocking thread. A failed save is dropped: the next check tries again.
    async fn persist(&self) {
        let Some(dir) = self.deps.dir.clone() else {
            return;
        };
        let state = self.state.clone();
        let _ = tokio::task::spawn_blocking(move || dir.save_update_state(&state)).await;
    }
}

// The mock server and the TEST ONLY signing helpers of `sqler-update`, included, not copied.
#[cfg(test)]
#[path = "../../sqler-update/tests/support/mod.rs"]
mod mock;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    use super::mock::{
        self, MockServer, Reply, Route, TEST_ONLY_SEED_A, dead_port_url, signed_envelope,
    };

    const FEED: &str = "/sqler-update.json";
    const SHORT: Duration = Duration::from_millis(100);
    const BOUND: Duration = Duration::from_secs(5);

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scratch() -> (Scratch, ConfigDir) {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!("sqler-app-update-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        let dir = ConfigDir::new(&p);
        (Scratch(p), dir)
    }

    fn schedule() -> CheckSchedule {
        CheckSchedule {
            startup_delay: SHORT,
            interval: Duration::from_secs(3600),
            first_retry: Duration::from_secs(3600),
            max_backoff: Duration::from_secs(3600),
        }
    }

    fn manifest(version: &str) -> String {
        let target = sqler_update::current_target().unwrap_or("x86_64-unknown-linux-gnu");
        format!(
            r#"{{"schema":1,"product":"sqler","version":"{version}","published":"2026-10-23","notes_url":"https://example.com/notes","platforms":{{"{target}":{{"url":"https://example.com/sqler","sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","size":42}}}}}}"#
        )
    }

    fn deps(url: String, schedule: CheckSchedule, dir: Option<ConfigDir>) -> Deps {
        let current = semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap();
        Deps {
            // TEST ONLY key: an end-to-end test cannot sign with the production key.
            make_client: Arc::new(move || {
                Ok(UpdateClient::new(&url, current.clone())?
                    .with_trusted_keys(vec![mock::trusted(&TEST_ONLY_SEED_A)]))
            }),
            configured: true,
            schedule,
            updates_dir: dir.as_ref().map(|d| d.root().join(UPDATES_DIR_NAME)),
            dir,
            clock: SystemTime::now,
            exe: None,
            package_dirs: Vec::new(),
            version_probe: Arc::new(mock::fake_probe),
            extract_app: Arc::new(|_, _| Err(UpdateError::ArchiveLayout)),
            bundle_check: Arc::new(|_| true),
            relaunch: Arc::new(|_| Ok(())),
        }
    }

    async fn serving(version: &str) -> MockServer {
        MockServer::start(vec![Route::new(
            FEED,
            Reply::ok(signed_envelope(&manifest(version), &TEST_ONLY_SEED_A)),
        )])
        .await
    }

    fn started(
        server_url: &str,
        schedule: CheckSchedule,
        dir: Option<ConfigDir>,
        state: UpdateState,
    ) -> (Arc<dyn UpdateService>, UnboundedReceiver<UpdateEvent>) {
        let startup = spawn(
            deps(format!("{server_url}{FEED}"), schedule, dir),
            state,
            &Handle::current(),
        );
        (startup.service.unwrap(), startup.events.unwrap())
    }

    async fn next(rx: &mut UnboundedReceiver<UpdateEvent>) -> UpdateEvent {
        tokio::time::timeout(BOUND, rx.recv())
            .await
            .expect("an event within the bound")
            .expect("the channel is open")
    }

    async fn until(mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + BOUND;
        while !done() {
            assert!(Instant::now() < deadline, "condition not met in time");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    async fn nothing_is_requested_while_the_setting_is_off() {
        let server = serving("9.9.9").await;
        let (service, mut rx) = started(&server.url(), schedule(), None, UpdateState::default());
        service.start(false);
        service.check_now();
        service.set_enabled(true);
        service.set_enabled(false);
        service.check_now();
        tokio::time::sleep(SHORT * 3).await;
        assert!(server.requests().is_empty());
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn nothing_is_requested_before_start() {
        let server = serving("9.9.9").await;
        let (service, mut rx) = started(&server.url(), schedule(), None, UpdateState::default());
        service.set_enabled(true);
        service.check_now();
        tokio::time::sleep(SHORT * 3).await;
        assert!(server.requests().is_empty());
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn one_request_after_the_startup_delay_with_a_bare_user_agent() {
        let server = serving("9.9.9").await;
        let schedule = CheckSchedule {
            startup_delay: Duration::from_millis(400),
            ..schedule()
        };
        let (service, mut rx) = started(&server.url(), schedule, None, UpdateState::default());
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(server.requests().is_empty(), "no request before start");
        service.start(true);
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(server.requests().is_empty(), "no request before the delay");
        assert_eq!(next(&mut rx).await, UpdateEvent::Started { manual: false });
        let UpdateEvent::Finished { manual, outcome } = next(&mut rx).await else {
            panic!("expected Finished");
        };
        assert!(!manual);
        assert_eq!(
            outcome,
            UpdateOutcome::Available {
                version: "9.9.9".into(),
                notes_url: "https://example.com/notes".into(),
            }
        );
        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!(request.method, "GET");
        assert_eq!(request.path, FEED);
        assert_eq!(request.query, None);
        assert_eq!(
            request.header("user-agent"),
            Some(format!("SQLer/{} (update-check)", env!("CARGO_PKG_VERSION")).as_str())
        );
        let mut names: Vec<&str> = request.headers.iter().map(|(k, _)| k.as_str()).collect();
        names.sort_unstable();
        assert_eq!(names, ["accept", "host", "user-agent"]);
    }

    #[tokio::test]
    async fn a_manual_check_runs_at_once_when_on() {
        let server = serving("9.9.9").await;
        let schedule = CheckSchedule {
            startup_delay: Duration::from_secs(3600),
            ..schedule()
        };
        let (service, mut rx) = started(&server.url(), schedule, None, UpdateState::default());
        service.start(true);
        service.check_now();
        assert_eq!(next(&mut rx).await, UpdateEvent::Started { manual: true });
        let UpdateEvent::Finished { manual, .. } = next(&mut rx).await else {
            panic!("expected Finished");
        };
        assert!(manual);
        assert_eq!(server.requests().len(), 1);
    }

    #[tokio::test]
    async fn an_unreachable_server_is_a_quiet_failure_that_is_counted() {
        let (_scratch, dir) = scratch();
        let url = dead_port_url().await;
        let (service, mut rx) =
            started(&url, schedule(), Some(dir.clone()), UpdateState::default());
        service.start(true);
        assert_eq!(next(&mut rx).await, UpdateEvent::Started { manual: false });
        let UpdateEvent::Finished { manual, outcome } = next(&mut rx).await else {
            panic!("expected Finished");
        };
        assert!(!manual);
        assert!(matches!(outcome, UpdateOutcome::Failed { .. }));
        let saved = dir.load_update_state();
        assert_eq!(saved.consecutive_failures, 1);
        assert!(saved.last_attempt.is_some());
    }

    #[tokio::test]
    async fn turning_the_setting_off_aborts_a_stalled_check() {
        let server = MockServer::start(vec![Route::new(FEED, Reply::stall())]).await;
        let (service, mut rx) = started(&server.url(), schedule(), None, UpdateState::default());
        service.start(true);
        assert_eq!(next(&mut rx).await, UpdateEvent::Started { manual: false });
        until(|| server.requests().len() == 1).await;
        service.set_enabled(false);
        tokio::time::timeout(BOUND, server.client_closed.notified())
            .await
            .expect("the client closed the connection");
        // No result is reported for the aborted check.
        tokio::time::sleep(SHORT).await;
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn a_recent_attempt_postpones_the_check() {
        let server = serving("9.9.9").await;
        let mut state = UpdateState::default();
        state.last_attempt = Some(unix_secs(SystemTime::now()));
        let (service, _rx) = started(&server.url(), schedule(), None, state);
        service.start(true);
        tokio::time::sleep(SHORT * 4).await;
        assert!(server.requests().is_empty());
    }

    #[tokio::test]
    async fn dismiss_is_persisted_and_handed_back_at_startup() {
        let (scratch, dir) = scratch();
        let server = serving("9.9.9").await;
        let (service, _rx) = started(
            &server.url(),
            schedule(),
            Some(dir.clone()),
            UpdateState::default(),
        );
        service.dismiss("1.2.3");
        until(|| dir.load_update_state().dismissed_version.as_deref() == Some("1.2.3")).await;

        let text = std::fs::read_to_string(dir.update_state_path()).unwrap();
        assert!(!text.contains('/') && !text.contains('\\'), "{text}");
        assert!(!text.contains(scratch.0.to_str().unwrap()));

        let startup = spawn(
            deps(format!("{}{FEED}", server.url()), schedule(), None),
            dir.load_update_state(),
            &Handle::current(),
        );
        assert_eq!(startup.dismissed.as_deref(), Some("1.2.3"));
    }

    #[tokio::test]
    async fn a_build_without_keys_reports_not_configured_and_sends_nothing() {
        let server = serving("9.9.9").await;
        let mut d = deps(format!("{}{FEED}", server.url()), schedule(), None);
        d.configured = false;
        let startup = spawn(d, UpdateState::default(), &Handle::current());
        let (service, mut rx) = (startup.service.unwrap(), startup.events.unwrap());
        assert!(!service.configured());
        service.start(true);
        service.check_now();
        assert_eq!(
            next(&mut rx).await,
            UpdateEvent::Finished {
                manual: true,
                outcome: UpdateOutcome::NotConfigured
            }
        );
        assert!(server.requests().is_empty());
    }

    // --- download and install (ADR-0078) -------------------------------------------------------------------
    //
    // The signed manifest only admits `https` artifact URLs, so a loopback server cannot be reached through
    // `check()`. These tests therefore build the offer by hand and drive the task methods against the mock; the
    // path from a signed check to a download is covered with a manifest whose URL is refused before any request.

    use sqler_update::{ArtifactKind, Download, artifact_file_name, current_target};

    type Log = Arc<Mutex<Vec<PathBuf>>>;

    fn target() -> &'static str {
        current_target().unwrap()
    }

    fn artifact_path(kind: ArtifactKind) -> &'static str {
        Box::leak(format!("/{}", artifact_file_name(target(), kind)).into_boxed_str())
    }

    fn offer(server: &MockServer, kind: ArtifactKind, body: &[u8], version: &str) -> UpdateOffer {
        let entry = Download {
            url: format!("{}{}", server.url(), artifact_path(kind)),
            sha256: mock::sha256_hex(body),
            size: body.len() as u64,
        };
        let (download, bundle) = match kind {
            ArtifactKind::Archive => (Some(entry), None),
            ArtifactKind::MacBundle => (None, Some(entry)),
        };
        UpdateOffer {
            version: semver::Version::parse(version).unwrap(),
            notes_url: "https://example.com/notes".into(),
            published: "2026-10-23".into(),
            download,
            bundle,
        }
    }

    struct Rig {
        task: Task,
        rx: UnboundedReceiver<UpdateEvent>,
        relaunched: Log,
        exe: PathBuf,
        config: ConfigDir,
        _scratch: Scratch,
    }

    impl Rig {
        fn updates(&self) -> PathBuf {
            self.config.root().join(UPDATES_DIR_NAME)
        }

        fn drain(&mut self) -> Vec<UpdateEvent> {
            let mut out = Vec::new();
            while let Ok(e) = self.rx.try_recv() {
                out.push(e);
            }
            out
        }

        /// Starts the download and waits for it, as the task loop does.
        async fn download(&mut self, offer: UpdateOffer, manual: bool) {
            self.task.begin_download(offer, manual);
            if let Some(running) = self.task.download.take() {
                self.task.held = running.handle.await.unwrap();
            }
        }
    }

    /// A task whose client points at `server`; `exe_rel` is the fake running program inside the scratch dir.
    fn rig(server: &MockServer, exe_rel: &str, bundle: bool) -> Rig {
        let (scratch, config) = scratch();
        let exe = scratch.0.join(exe_rel);
        if bundle {
            let app = exe.ancestors().nth(3).unwrap().to_path_buf();
            mock::fake_app_tree(&app, "1.0.0");
        } else {
            std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
            std::fs::write(&exe, mock::fake_program("1.0.0")).unwrap();
        }
        let relaunched: Log = Arc::new(Mutex::new(Vec::new()));
        let mut d = deps(
            format!("{}{FEED}", server.url()),
            schedule(),
            Some(config.clone()),
        );
        d.exe = Some(exe.clone());
        let log = Arc::clone(&relaunched);
        d.relaunch = Arc::new(move |p| {
            log.lock().unwrap().push(p.to_path_buf());
            Ok(())
        });
        if bundle {
            d.extract_app = Arc::new(|_dmg, dest| {
                mock::fake_app_tree(dest, "9.9.9");
                Ok(())
            });
        }
        let client = UpdateClient::new(
            &format!("{}{FEED}", server.url()),
            semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap(),
        )
        .unwrap()
        .with_trusted_keys(vec![mock::trusted(&TEST_ONLY_SEED_A)]);
        let (events, rx) = unbounded_channel();
        let task = Task {
            deps: d,
            state: UpdateState::default(),
            events,
            client: Arc::new(OnceCell::new_with(Some(client))),
            enabled: true,
            started: true,
            deadline: None,
            inflight: None,
            download: None,
            held: None,
            installed: None,
        };
        Rig {
            task,
            rx,
            relaunched,
            exe,
            config,
            _scratch: scratch,
        }
    }

    async fn archive_server(body: Vec<u8>) -> MockServer {
        MockServer::start(vec![Route::new(
            artifact_path(ArtifactKind::Archive),
            Reply::bytes(body),
        )])
        .await
    }

    fn new_archive() -> Vec<u8> {
        mock::tar_xz_with(&[mock::Entry::File("sqler", &mock::fake_program("9.9.9"))])
    }

    fn is_download(e: &UpdateEvent, want: &DownloadEvent) -> bool {
        matches!(e, UpdateEvent::Download(d) if d == want)
    }

    fn v(version: &str) -> String {
        version.to_owned()
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_archive_is_downloaded_verified_installed_and_relaunched() {
        let body = new_archive();
        let server = archive_server(body.clone()).await;
        let mut rig = rig(&server, "bin/sqler", false);
        let o = offer(&server, ArtifactKind::Archive, &body, "9.9.9");
        rig.download(o, true).await;
        let events = rig.drain();
        assert!(events.iter().any(|e| is_download(
            e,
            &DownloadEvent::Verified {
                version: v("9.9.9")
            }
        )));
        assert!(events.iter().all(|e| matches!(
            e,
            UpdateEvent::Download(DownloadEvent::Progress { .. } | DownloadEvent::Verified { .. })
        )));
        assert!(rig.task.held.is_some());
        assert_eq!(server.requests().len(), 1);

        rig.task.install("9.9.9").await;
        let events = rig.drain();
        assert_eq!(
            events,
            [
                UpdateEvent::Download(DownloadEvent::Installing {
                    version: v("9.9.9")
                }),
                UpdateEvent::Download(DownloadEvent::Restarting {
                    version: v("9.9.9"),
                    relaunch: true
                }),
            ]
        );
        assert_eq!(
            std::fs::read(&rig.exe).unwrap(),
            mock::fake_program("9.9.9")
        );
        assert_eq!(*rig.relaunched.lock().unwrap(), [rig.exe.clone()]);

        // The installed version is never downloaded again in this run.
        let again = offer(&server, ArtifactKind::Archive, &body, "9.9.9");
        rig.task.begin_download(again, true);
        assert!(rig.task.download.is_none());
        assert_eq!(server.requests().len(), 1);
    }

    #[test]
    fn starting_a_download_clears_the_other_versions() {
        let (scratch, _dir) = scratch();
        let updates = scratch.0.join("updates");
        for old in ["1.0.0", "1.1.0", "1.2.0"] {
            std::fs::create_dir_all(updates.join(old)).unwrap();
            std::fs::write(updates.join(old).join("a.part"), b"x").unwrap();
        }
        clear_other_versions(&updates.join("1.2.0"));
        assert!(updates.join("1.2.0/a.part").exists());
        assert!(!updates.join("1.0.0").exists() && !updates.join("1.1.0").exists());
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn a_bundle_is_swapped_whole_and_the_app_path_is_relaunched() {
        let body = b"not really a disk image".to_vec();
        let server = MockServer::start(vec![Route::new(
            artifact_path(ArtifactKind::MacBundle),
            Reply::bytes(body.clone()),
        )])
        .await;
        let mut rig = rig(&server, "SQLer.app/Contents/MacOS/sqler", true);
        let o = offer(&server, ArtifactKind::MacBundle, &body, "9.9.9");
        rig.download(o, true).await;
        assert!(rig.task.held.is_some());
        assert_eq!(
            server.requests()[0].path,
            artifact_path(ArtifactKind::MacBundle)
        );

        rig.task.install("9.9.9").await;
        let events = rig.drain();
        assert!(is_download(
            events.last().unwrap(),
            &DownloadEvent::Restarting {
                version: v("9.9.9"),
                relaunch: true
            }
        ));
        let app = rig.exe.ancestors().nth(3).unwrap().to_path_buf();
        assert_eq!(
            std::fs::read(&rig.exe).unwrap(),
            mock::fake_program("9.9.9")
        );
        assert_eq!(*rig.relaunched.lock().unwrap(), [app]);
    }

    #[tokio::test]
    async fn a_wrong_hash_or_size_is_refused_and_leaves_no_file() {
        let good = b"0123456789".to_vec();
        for reply in [
            Reply::bytes(b"9876543210".to_vec()),
            Reply::bytes(b"0123".to_vec()),
        ] {
            let server = MockServer::start(vec![Route::new(
                artifact_path(ArtifactKind::Archive),
                reply,
            )])
            .await;
            let mut rig = rig(&server, "bin/sqler", false);
            let o = offer(&server, ArtifactKind::Archive, &good, "9.9.9");
            rig.download(o, true).await;
            let events = rig.drain();
            assert!(matches!(
                events.last(),
                Some(UpdateEvent::Download(DownloadEvent::Refused { .. }))
            ));
            assert!(rig.task.held.is_none());
            let left: Vec<_> = std::fs::read_dir(rig.updates().join("9.9.9"))
                .map(Iterator::collect)
                .unwrap_or_default();
            assert!(left.is_empty(), "{left:?}");
        }
    }

    #[tokio::test]
    async fn a_missing_platform_entry_is_refused_without_a_request() {
        let server = archive_server(Vec::new()).await;
        let mut rig = rig(&server, "bin/sqler", false);
        let mut o = offer(&server, ArtifactKind::Archive, b"x", "9.9.9");
        o.download = None;
        rig.download(o, true).await;
        assert!(matches!(
            rig.drain().last(),
            Some(UpdateEvent::Download(DownloadEvent::Refused { .. }))
        ));
        assert!(server.requests().is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_file_changed_after_verification_is_refused_and_deleted() {
        let body = new_archive();
        let server = archive_server(body.clone()).await;
        let mut rig = rig(&server, "bin/sqler", false);
        let o = offer(&server, ArtifactKind::Archive, &body, "9.9.9");
        rig.download(o, true).await;
        rig.drain();
        let path = rig.task.held.as_ref().unwrap().path.clone();
        let mut tampered = std::fs::read(&path).unwrap();
        tampered[0] ^= 0xFF;
        std::fs::write(&path, tampered).unwrap();

        rig.task.install("9.9.9").await;
        let events = rig.drain();
        assert!(matches!(
            events.last(),
            Some(UpdateEvent::Download(DownloadEvent::Refused { .. }))
        ));
        assert!(!path.exists());
        assert!(rig.task.held.is_none());
        assert_eq!(
            std::fs::read(&rig.exe).unwrap(),
            mock::fake_program("1.0.0")
        );
        assert!(rig.relaunched.lock().unwrap().is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_package_managed_install_is_manual_and_names_the_verified_file() {
        let body = new_archive();
        let server = archive_server(body.clone()).await;
        let mut rig = rig(&server, "bin/sqler", false);
        rig.task.deps.package_dirs = vec![rig.exe.parent().unwrap().to_path_buf()];
        let o = offer(&server, ArtifactKind::Archive, &body, "9.9.9");
        rig.download(o, true).await;
        let path = rig.task.held.as_ref().unwrap().path.clone();
        rig.drain();

        rig.task.install("9.9.9").await;
        let events = rig.drain();
        let Some(UpdateEvent::Download(DownloadEvent::Manual {
            reason,
            path: shown,
            ..
        })) = events.last()
        else {
            panic!("expected Manual, got {events:?}");
        };
        assert_eq!(reason, sqler_update::ManualReason::PackageManager.note());
        assert_eq!(shown, &path.display().to_string());
        assert!(path.is_file());
        assert_eq!(
            std::fs::read(&rig.exe).unwrap(),
            mock::fake_program("1.0.0")
        );
        assert!(rig.relaunched.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn installing_without_a_verified_file_fails_quietly() {
        let server = archive_server(Vec::new()).await;
        let mut rig = rig(&server, "bin/sqler", false);
        rig.task.install("9.9.9").await;
        assert_eq!(
            rig.drain(),
            [UpdateEvent::Download(DownloadEvent::InstallFailed {
                version: v("9.9.9"),
                reason: "No verified update is ready to install.".into()
            })]
        );
    }

    #[tokio::test]
    async fn a_dismissed_version_is_not_downloaded_automatically_but_on_demand() {
        let body = new_archive();
        let server = archive_server(body.clone()).await;
        let mut rig = rig(&server, "bin/sqler", false);
        rig.task.state.dismissed_version = Some("9.9.9".into());
        let o = offer(&server, ArtifactKind::Archive, &body, "9.9.9");
        rig.download(o.clone(), false).await;
        assert!(rig.task.held.is_none());
        assert_eq!(rig.drain().len(), 0);
        assert!(server.requests().is_empty());
        rig.download(o, true).await;
        assert!(rig.task.held.is_some());
    }

    #[tokio::test]
    async fn turning_the_setting_off_aborts_the_download_and_removes_updates() {
        let server = MockServer::start(vec![Route::new(
            artifact_path(ArtifactKind::Archive),
            Reply::stall(),
        )])
        .await;
        let mut rig = rig(&server, "bin/sqler", false);
        std::fs::create_dir_all(rig.updates().join("9.9.8")).unwrap();
        std::fs::write(rig.updates().join("9.9.8").join("old"), b"x").unwrap();
        let o = offer(&server, ArtifactKind::Archive, b"abc", "9.9.9");
        rig.task.begin_download(o, true);
        until(|| server.requests().len() == 1).await;
        rig.task.set_enabled(false, true).await;
        assert!(rig.task.download.is_none());
        assert!(rig.task.held.is_none());
        tokio::time::timeout(BOUND, server.client_closed.notified())
            .await
            .expect("the client closed the connection");
        assert!(!rig.updates().exists());
    }

    #[tokio::test]
    async fn a_check_that_finds_a_release_downloads_it_and_reports_a_refusal() {
        // The manifest URL is not a trusted release address, so the download is refused before any request.
        let server = serving("9.9.9").await;
        let (scratch, config) = scratch();
        let exe = scratch.0.join("bin").join("sqler");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, mock::fake_program("1.0.0")).unwrap();
        let mut d = deps(format!("{}{FEED}", server.url()), schedule(), Some(config));
        d.exe = Some(exe);
        let startup = spawn(d, UpdateState::default(), &Handle::current());
        let (service, mut rx) = (startup.service.unwrap(), startup.events.unwrap());
        service.start(true);
        assert_eq!(next(&mut rx).await, UpdateEvent::Started { manual: false });
        assert!(matches!(
            next(&mut rx).await,
            UpdateEvent::Finished {
                outcome: UpdateOutcome::Available { .. },
                ..
            }
        ));
        let UpdateEvent::Download(DownloadEvent::Refused { version, reason }) = next(&mut rx).await
        else {
            panic!("expected Refused");
        };
        assert_eq!(version, "9.9.9");
        assert_eq!(reason, UpdateError::UntrustedUrl.to_string());
        assert_eq!(server.requests().len(), 1, "only the feed was requested");
    }

    #[tokio::test]
    async fn a_bad_signature_downloads_nothing() {
        let server = MockServer::start(vec![Route::new(
            FEED,
            Reply::ok(signed_envelope(&manifest("9.9.9"), &mock::TEST_ONLY_SEED_B)),
        )])
        .await;
        let (scratch, config) = scratch();
        let exe = scratch.0.join("sqler");
        std::fs::write(&exe, mock::fake_program("1.0.0")).unwrap();
        let mut d = deps(format!("{}{FEED}", server.url()), schedule(), Some(config));
        d.exe = Some(exe);
        let startup = spawn(d, UpdateState::default(), &Handle::current());
        let (service, mut rx) = (startup.service.unwrap(), startup.events.unwrap());
        service.start(true);
        assert_eq!(next(&mut rx).await, UpdateEvent::Started { manual: false });
        assert!(matches!(
            next(&mut rx).await,
            UpdateEvent::Finished {
                outcome: UpdateOutcome::Failed { .. },
                ..
            }
        ));
        tokio::time::sleep(SHORT * 2).await;
        assert!(rx.try_recv().is_err());
        assert_eq!(server.requests().len(), 1);
    }
}
