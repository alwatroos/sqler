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

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sqler_config::{ConfigDir, UpdateState};
use sqler_ui::update::{UpdateEvent, UpdateOutcome, UpdateService, UpdateStartup};
use sqler_update::{
    CheckOutcome, CheckSchedule, DEFAULT_FEED_URL, UpdateClient, UpdateError, embedded_keys,
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
}

/// The production service: the real feed, the compiled-in keys, the default schedule.
pub(crate) fn startup(dir: Option<&ConfigDir>, rt: &Handle) -> UpdateStartup {
    let Ok(current) = semver::Version::parse(env!("CARGO_PKG_VERSION")) else {
        return UpdateStartup::unavailable();
    };
    let state = dir.map(ConfigDir::load_update_state).unwrap_or_default();
    let deps = Deps {
        make_client: Arc::new(move || UpdateClient::new(DEFAULT_FEED_URL, current.clone())),
        configured: !embedded_keys().is_empty(),
        schedule: CheckSchedule::default(),
        dir: dir.cloned(),
        clock: SystemTime::now,
    };
    spawn(deps, state, rt)
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
}

struct Inflight {
    handle: JoinHandle<CheckResult>,
    manual: bool,
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
        }
    }
    task.abort_inflight();
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
                self.set_enabled(on, true);
            }
            Control::SetEnabled(on) => self.set_enabled(on, self.started),
            Control::CheckNow => self.begin(true),
            Control::Dismiss(version) => {
                self.state.dismissed_version = Some(version);
                self.persist().await;
            }
        }
    }

    /// On arms the timer (once, `arm` false keeps it idle until `Start`); off stops the timer and aborts the check.
    fn set_enabled(&mut self, on: bool, arm: bool) {
        if on {
            self.enabled = true;
            if arm && self.deadline.is_none() && self.inflight.is_none() {
                self.deadline = Some(Instant::now() + self.deps.schedule.startup_delay);
            }
        } else {
            self.enabled = false;
            self.deadline = None;
            self.abort_inflight();
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
        let outcome = match result {
            Ok(CheckOutcome::UpToDate { latest }) => UpdateOutcome::UpToDate {
                latest: latest.to_string(),
            },
            Ok(CheckOutcome::Available(offer)) => UpdateOutcome::Available {
                version: offer.version.to_string(),
                notes_url: offer.notes_url,
            },
            Err(err) => UpdateOutcome::Failed {
                message: err.to_string(),
            },
        };
        self.send(UpdateEvent::Finished { manual, outcome });
        if self.enabled {
            let wait = self
                .deps
                .schedule
                .delay_after(self.state.consecutive_failures);
            self.deadline = Some(Instant::now() + wait);
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
    use std::path::PathBuf;
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
            dir,
            clock: SystemTime::now,
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
}
