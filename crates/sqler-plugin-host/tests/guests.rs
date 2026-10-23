// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! End-to-end tests with real guest components (`plugins/fixtures/{smoke,probe}`), built by
//! `cargo run -p xtask -- plugins build`.
//!
//! They run when `SQLER_PLUGIN_FIXTURES` points at the staged directory (`target/plugins/<profile>`, relative paths
//! are taken from the workspace root). Without it each test prints a `skipped:` line and passes, so a plain
//! `cargo test` needs no wasm toolchain; `SQLER_TEST_REQUIRE_PLUGINS=1` turns a missing fixture into a failure (the CI
//! `plugins` job sets both). Every await is bounded by a 30 s hang guard; the only network is 127.0.0.1.
#![cfg(feature = "cl")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;
#[path = "guests/completion.rs"]
mod completion;
#[path = "guests/data.rs"]
mod data;
#[path = "guests/driver.rs"]
mod driver;
#[path = "guests/exporter.rs"]
mod exporter;
#[path = "guests/exporter_markdown.rs"]
mod exporter_markdown;
#[path = "guests/hello_panel.rs"]
mod hello_panel;

use std::convert::Infallible;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use sqler_plugin_api::{
    Capability, CapabilitySet, CommandContext, ContributionId, PluginId, SettingValue, Widget,
    WidgetError, WidgetEvent,
};
use sqler_plugin_host::{
    CallError, DataService, EgressFactory, ErrorCode, GrantSource, HostOptions, MemorySettings,
    PluginData, PluginDirs, PluginHost, PluginState, PluginUi, QueryApprover, Services, UiSink,
    discover,
};

use common::{Grants, Scratch, block_on, capture, plugin_lines, runtime};

/// The staged fixture directory, or `None` (after saying why) when the test must be skipped.
fn fixtures(test: &str) -> Option<PathBuf> {
    let required = std::env::var("SQLER_TEST_REQUIRE_PLUGINS").is_ok_and(|v| v == "1");
    let dir = std::env::var_os("SQLER_PLUGIN_FIXTURES").map(PathBuf::from);
    let resolved = dir.map(|d| {
        if d.is_absolute() {
            d
        } else {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(d)
        }
    });
    let complete = resolved.as_ref().filter(|d| {
        d.join("probe/probe.wasm").is_file()
            && d.join("smoke/smoke.wasm").is_file()
            && d.join("exporter-fixture/exporter.wasm").is_file()
    });
    if let Some(d) = complete {
        return Some(d.clone());
    }
    assert!(
        !required,
        "{test}: SQLER_TEST_REQUIRE_PLUGINS=1 but SQLER_PLUGIN_FIXTURES is unset or lacks the built probe, smoke and exporter \
         plugins (run `cargo run -p xtask -- plugins build`)"
    );
    eprintln!(
        "skipped: {test}: set SQLER_PLUGIN_FIXTURES=target/plugins/<profile> to run the guest tests"
    );
    None
}

/// What the grant source approves for the probe.
enum Approve {
    /// Everything the manifest declares.
    Declared,
    /// Nothing at all.
    Nothing,
    /// Exactly these capability strings.
    Only(&'static [&'static str]),
}

struct Opts {
    approve: Approve,
    /// A local HTTP server; its port is granted as `net.http:localhost:<port>` and the `url` setting points at it.
    server: bool,
    command: Duration,
    /// Deadline of a status call (30 s unless a test wants the host to drop a call that waits on the database).
    status: Duration,
    /// Services the host starts with; `data` adds the database on top.
    services: Services,
    /// A SQLite database lent to the probe through `DataService`, with a scripted approver.
    data: Option<data::DataOpts>,
    /// Overrides the default timeout of a `query.run` without `timeout-ms`.
    query_timeout: Option<Duration>,
}

impl Default for Opts {
    fn default() -> Self {
        Self {
            approve: Approve::Declared,
            server: false,
            command: Duration::from_secs(30),
            status: Duration::from_secs(30),
            services: Services::new(),
            data: None,
            query_timeout: None,
        }
    }
}

struct World {
    _scratch: Scratch,
    rt: tokio::runtime::Runtime,
    host: PluginHost,
    id: PluginId,
    /// Requests the local server answered (read by the `net` tests only).
    #[cfg_attr(not(feature = "net"), allow(dead_code))]
    hits: Arc<AtomicUsize>,
    /// Whether the escaping symlinks inside `data` could be created (not everywhere, for example Windows without
    /// the privilege).
    links: bool,
    grants: Arc<Grants>,
    key: sqler_plugin_api::GrantKey,
    /// The database lent to the probe, when `Opts.data` asked for one.
    data: Option<Arc<data::Sqlite>>,
}

#[cfg(unix)]
fn symlink(target: &str, link: &Path, _dir: bool) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn symlink(target: &str, link: &Path, dir: bool) -> std::io::Result<()> {
    if dir {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    }
}

fn copy_plugin(fixtures: &Path, name: &str, to: &Path) {
    let from = fixtures.join(name);
    fs::create_dir_all(to).unwrap();
    for file in ["sqler.toml", &format!("{name}.wasm")] {
        fs::copy(from.join(file), to.join(file)).unwrap();
    }
}

async fn serve(hits: Arc<AtomicUsize>) -> u16 {
    use hyper::service::service_fn;
    use hyper_util::rt::TokioIo;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let hits = Arc::clone(&hits);
            tokio::spawn(async move {
                let svc = service_fn(move |_req: hyper::Request<hyper::body::Incoming>| {
                    hits.fetch_add(1, Ordering::SeqCst);
                    async {
                        Ok::<_, Infallible>(hyper::Response::new(http_body_util::Full::new(
                            bytes::Bytes::from_static(b"ok"),
                        )))
                    }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), svc)
                    .await;
            });
        }
    });
    port
}

fn egress() -> Arc<dyn EgressFactory> {
    #[cfg(feature = "net")]
    {
        Arc::new(sqler_plugin_host::net::NetEgressFactory::new())
    }
    #[cfg(not(feature = "net"))]
    {
        Arc::new(sqler_plugin_host::NoEgress)
    }
}

/// A host over a scratch copy of the staged plugin `name`.
fn world(label: &str, fixtures: &Path, name: &str, opts: &Opts) -> World {
    let scratch = Scratch::new(&format!("guest-{label}"));
    let dir = scratch.path().join(name);
    copy_plugin(fixtures, name, &dir);
    // `fs.read:data` needs the directory; `escape.txt` sits one level above it, inside the plugin directory.
    fs::create_dir_all(dir.join("data")).unwrap();
    fs::write(dir.join("data/hello.txt"), "hello from data").unwrap();
    fs::write(dir.join("escape.txt"), "outside the data scope").unwrap();
    // Symlinks inside the granted scope that lead out of it: a file and a directory.
    let links = symlink("../escape.txt", &dir.join("data/link.txt"), false).is_ok()
        && symlink("..", &dir.join("data/up"), true).is_ok();

    let rt = runtime();
    let hits = Arc::new(AtomicUsize::new(0));
    let port = opts.server.then(|| block_on(&rt, serve(Arc::clone(&hits))));
    if let Some(port) = port {
        let manifest = dir.join("sqler.toml");
        let text = fs::read_to_string(&manifest).unwrap();
        let patched = text.replace(
            "\"fs.read:plugin\"]",
            &format!("\"fs.read:plugin\", \"net.http:localhost:{port}\"]"),
        );
        assert_ne!(text, patched, "the probe manifest changed shape");
        fs::write(&manifest, patched).unwrap();
    }

    let d = discover(&PluginDirs {
        dev: vec![dir],
        ..PluginDirs::default()
    });
    assert!(d.problems.is_empty(), "{:?}", d.problems);
    let plugin = &d.plugins[0];
    let id = plugin.manifest.id().clone();
    let grants = Arc::new(Grants::default());
    let key = plugin.key.clone();
    let mut services = opts.services.clone();
    let data = opts.data.as_ref().map(|d| {
        let sqlite = block_on(&rt, data::Sqlite::new(label, d));
        let mut lent = PluginData::new(Arc::clone(&sqlite) as Arc<dyn DataService>);
        if let Some(approver) = &d.approver {
            lent = lent.with_approver(Arc::clone(approver) as Arc<dyn QueryApprover>);
        }
        services.insert(Arc::new(lent));
        sqlite
    });
    match &opts.approve {
        Approve::Declared => grants.approve_declared(&d),
        Approve::Nothing => {}
        Approve::Only(list) => {
            let set: CapabilitySet = list
                .iter()
                .map(|c| c.parse::<Capability>().unwrap())
                .collect();
            grants.approve(&plugin.key, set);
        }
    }

    let mut limits = common::test_limits(Duration::from_secs(30));
    limits.deadlines.status = opts.status;
    if let Some(t) = opts.query_timeout {
        limits.query.timeout_default = t;
    }
    limits.deadlines.command = opts.command;
    let host = PluginHost::new(HostOptions {
        runtime: rt.handle().clone(),
        grants: Arc::clone(&grants) as Arc<dyn GrantSource>,
        settings: Arc::new(MemorySettings::default()),
        egress: egress(),
        limits,
        services,
    });
    let problems = host.load(d);
    assert_eq!(problems.len(), 0, "{problems:?}");
    if let Some(port) = port {
        host.set_setting(
            &id,
            "url",
            SettingValue::Text(format!("http://localhost:{port}/")),
        )
        .unwrap();
    }
    World {
        _scratch: scratch,
        rt,
        host,
        id,
        hits,
        links,
        grants,
        key,
        data,
    }
}

impl World {
    /// The status text, or `None` when the guest has nothing to show (an empty text counts as nothing).
    fn status_opt(&self, item: &str) -> Option<String> {
        block_on(&self.rt, self.host.status(&self.id, &cid(item))).unwrap()
    }

    fn status(&self, item: &str) -> String {
        self.status_opt(item)
            .unwrap_or_else(|| panic!("no status for {item}"))
    }

    fn command(&self, command: &str) -> Result<(), CallError> {
        self.command_in(command, CommandContext::None)
    }

    fn command_in(&self, command: &str, context: CommandContext) -> Result<(), CallError> {
        block_on(
            &self.rt,
            self.host.run_command(&self.id, &cid(command), context),
        )
    }

    fn set(&self, key: &str, value: &str) {
        self.host
            .set_setting(&self.id, key, SettingValue::Text(value.to_owned()))
            .unwrap();
    }

    fn done(self) {
        self.host.shutdown();
    }
}

#[test]
fn smoke_init_logs_one_line() {
    let Some(fx) = fixtures("smoke_init_logs_one_line") else {
        return;
    };
    let w = world("smoke", &fx, "smoke", &Opts::default());
    let ((), lines) = capture(|| {
        block_on(&w.rt, w.host.init(&w.id)).unwrap();
    });
    let logged = plugin_lines(&lines);
    assert_eq!(logged.len(), 1, "{lines:?}");
    assert_eq!(logged[0].message, "smoke: init");
    assert_eq!(logged[0].plugin.as_deref(), Some("smoke"));
    assert_eq!(w.host.plugins()[0].state, PluginState::Live);
    w.done();
}

#[test]
fn probe_ok_command_and_init_log() {
    let Some(fx) = fixtures("probe_ok_command_and_init_log") else {
        return;
    };
    let w = world("ok", &fx, "probe", &Opts::default());
    let (r, lines) = capture(|| w.command("ok"));
    r.unwrap();
    assert!(
        plugin_lines(&lines)
            .iter()
            .any(|l| l.message == "probe: init")
    );
    // A declared command the guest does not know is a guest error, not a fault.
    match w.command("unknown") {
        Err(CallError::Guest(e)) => assert_eq!(e.code, ErrorCode::NotFound),
        other => panic!("{other:?}"),
    }
    assert_eq!(w.host.plugins()[0].state, PluginState::Live);
    w.done();
}

#[test]
fn panic_is_a_trap_with_a_stderr_line_and_the_host_survives() {
    let Some(fx) = fixtures("panic_is_a_trap") else {
        return;
    };
    let w = world("panic", &fx, "probe", &Opts::default());
    let (r, lines) = capture(|| w.command("panic"));
    assert!(matches!(r, Err(CallError::Trap { .. })), "{r:?}");
    let logged = plugin_lines(&lines);
    assert!(
        logged
            .iter()
            .any(|l| l.message.contains("deliberate panic") && l.level == tracing::Level::WARN),
        "{lines:?}"
    );
    // The instance was dropped; the next call relinks and works.
    w.command("ok").unwrap();
    w.done();
}

#[test]
fn trap_is_a_trap() {
    let Some(fx) = fixtures("trap_is_a_trap") else {
        return;
    };
    let w = world("trap", &fx, "probe", &Opts::default());
    assert!(matches!(w.command("trap"), Err(CallError::Trap { .. })));
    w.done();
}

#[test]
fn alloc_hits_the_memory_cap() {
    let Some(fx) = fixtures("alloc_hits_the_memory_cap") else {
        return;
    };
    let w = world("alloc", &fx, "probe", &Opts::default());
    match w.command("alloc") {
        Err(CallError::MemoryLimit { limit }) => assert_eq!(limit, 64 * 1024 * 1024),
        other => panic!("{other:?}"),
    }
    w.command("ok").unwrap();
    w.done();
}

#[test]
fn spin_times_out() {
    let Some(fx) = fixtures("spin_times_out") else {
        return;
    };
    let opts = Opts {
        command: Duration::from_millis(300),
        ..Opts::default()
    };
    let w = world("spin", &fx, "probe", &opts);
    w.command("ok").unwrap();
    assert!(matches!(w.command("spin"), Err(CallError::Timeout { .. })));
    w.done();
}

#[test]
fn a_guest_blocked_in_a_host_wait_is_interrupted() {
    let Some(fx) = fixtures("a_guest_blocked_in_a_host_wait") else {
        return;
    };
    let opts = Opts {
        command: Duration::from_millis(300),
        ..Opts::default()
    };
    let w = world("sleep", &fx, "probe", &opts);
    // Warm up so the one hour sleep, not the compile, meets the deadline.
    w.command("ok").unwrap();
    assert!(matches!(w.command("sleep"), Err(CallError::Timeout { .. })));
    w.done();
}

#[test]
fn three_consecutive_faults_disable_the_plugin() {
    let Some(fx) = fixtures("three_consecutive_faults") else {
        return;
    };
    let w = world("faults", &fx, "probe", &Opts::default());
    for _ in 0..3 {
        assert!(matches!(w.command("trap"), Err(CallError::Trap { .. })));
    }
    assert!(matches!(w.command("ok"), Err(CallError::Disabled)));
    w.host.enable(&w.id);
    w.command("ok").unwrap();
    w.done();
}

#[test]
fn environment_is_empty() {
    let Some(fx) = fixtures("environment_is_empty") else {
        return;
    };
    let w = world("env", &fx, "probe", &Opts::default());
    assert_eq!(w.status("env"), "vars=0 args=0");
    w.done();
}

#[test]
fn scoped_reads_work_and_escapes_do_not() {
    let Some(fx) = fixtures("scoped_reads") else {
        return;
    };
    let w = world("reads", &fx, "probe", &Opts::default());
    assert_eq!(w.status("read-data"), "hello from data");
    assert!(w.status("read-plugin").contains("id = \"probe\""));
    // The file exists one level above `data`: only the sandbox keeps it out.
    let escape = w.status("read-escape");
    assert!(escape.starts_with("err:"), "{escape}");
    let root = w.status("read-root");
    assert!(root.starts_with("err:"), "{root}");
    w.done();
}

#[test]
fn a_symlink_inside_a_granted_scope_cannot_lead_out_of_it() {
    let Some(fx) = fixtures("symlink_escape") else {
        return;
    };
    let w = world("symlink", &fx, "probe", &Opts::default());
    if !w.links {
        eprintln!("skipped: symlink_escape: this platform cannot create symlinks here");
        w.done();
        return;
    }
    // The targets exist and the process could read them: only the sandbox refuses.
    assert_eq!(w.status("read-data"), "hello from data");
    for item in ["read-link", "read-linkdir"] {
        let r = w.status(item);
        assert!(r.starts_with("err:"), "{item}: {r}");
        assert!(!r.contains("outside the data scope"), "{item}: {r}");
    }
    w.done();
}

#[test]
fn a_declared_but_unapproved_scope_gets_no_preopen() {
    let Some(fx) = fixtures("unapproved_scope") else {
        return;
    };
    for (test, approve) in [
        ("none", Approve::Nothing),
        ("plugin-only", Approve::Only(&["fs.read:plugin"])),
    ] {
        let opts = Opts {
            approve,
            ..Opts::default()
        };
        let w = world(&format!("unapproved-{test}"), &fx, "probe", &opts);
        let data = w.status("read-data");
        assert!(data.starts_with("err:"), "{test}: {data}");
        let plugin = w.status("read-plugin");
        assert_eq!(
            plugin.starts_with("err:"),
            test == "none",
            "{test}: {plugin}"
        );
        w.done();
    }
}

#[test]
fn settings_reach_the_guest_read_only() {
    let Some(fx) = fixtures("settings_reach_the_guest") else {
        return;
    };
    let w = world("setting", &fx, "probe", &Opts::default());
    // An unset setting reads as empty text, which the host reports as nothing to show.
    assert_eq!(w.status_opt("setting"), None);
    w.host
        .set_setting(&w.id, "url", SettingValue::Text("http://x/".to_owned()))
        .unwrap();
    assert_eq!(w.status("setting"), "http://x/");
    w.done();
}

#[test]
#[cfg(feature = "net")]
fn http_to_the_granted_host_and_port_passes() {
    let Some(fx) = fixtures("http_allowed") else {
        return;
    };
    let opts = Opts {
        server: true,
        ..Opts::default()
    };
    let w = world("http", &fx, "probe", &opts);
    assert_eq!(w.status("http"), "status=200");
    assert_eq!(w.hits.load(Ordering::SeqCst), 1);
    w.done();
}

#[test]
#[cfg(feature = "net")]
fn http_to_another_host_is_denied_and_never_leaves() {
    let Some(fx) = fixtures("http_denied") else {
        return;
    };
    let opts = Opts {
        server: true,
        ..Opts::default()
    };
    let w = world("http-other", &fx, "probe", &opts);
    let other = w.status("http-other");
    assert!(
        other.starts_with("err:") && other.contains("HttpRequestDenied"),
        "{other}"
    );
    assert_eq!(w.hits.load(Ordering::SeqCst), 0);
    w.done();
}

#[test]
#[cfg(feature = "net")]
fn http_without_an_approved_grant_is_denied() {
    let Some(fx) = fixtures("http_unapproved") else {
        return;
    };
    let opts = Opts {
        server: true,
        approve: Approve::Only(&["fs.read:data", "fs.read:plugin"]),
        ..Opts::default()
    };
    let w = world("http-unapproved", &fx, "probe", &opts);
    let denied = w.status("http");
    assert!(denied.contains("HttpRequestDenied"), "{denied}");
    assert_eq!(w.hits.load(Ordering::SeqCst), 0);
    w.done();
}

#[test]
fn log_flood_is_rate_limited() {
    let Some(fx) = fixtures("log_flood") else {
        return;
    };
    let w = world("flood", &fx, "probe", &Opts::default());
    let (r, lines) = capture(|| w.command("log-flood"));
    r.unwrap();
    let logged = plugin_lines(&lines);
    let flood = logged
        .iter()
        .filter(|l| l.message.starts_with("flood "))
        .count();
    let notices = logged
        .iter()
        .filter(|l| l.message.contains("suppressed"))
        .count();
    // The bucket holds 100 and refills at 20 per second; 1000 lines in a burst never get through, however slow the
    // machine is. The bound is loose on purpose (no wall-clock assertions).
    assert!((50..400).contains(&flood), "{flood} flood lines");
    assert!(notices <= 5, "{notices} suppression notices");
    w.done();
}

#[test]
fn nothing_approved_still_loads_and_runs() {
    let Some(fx) = fixtures("nothing_approved") else {
        return;
    };
    let opts = Opts {
        approve: Approve::Nothing,
        ..Opts::default()
    };
    let w = world("nothing", &fx, "probe", &opts);
    w.command("ok").unwrap();
    assert!(w.host.plugins()[0].effective.is_empty());
    w.done();
}

fn cid(s: &str) -> ContributionId {
    s.parse().unwrap()
}

/// Records what the guest pushed through the `ui` import.
#[derive(Default)]
struct Pushed(std::sync::Mutex<Vec<String>>);

impl Pushed {
    fn count(&self, prefix: &str) -> usize {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter(|l| l.starts_with(prefix))
            .count()
    }
}

impl UiSink for Pushed {
    fn refresh_panel(&self, plugin: &PluginId, panel: &ContributionId) {
        self.0
            .lock()
            .unwrap()
            .push(format!("refresh {plugin}/{panel}"));
    }

    fn set_status(&self, plugin: &PluginId, item: &ContributionId, text: String) {
        self.0
            .lock()
            .unwrap()
            .push(format!("status {plugin}/{item} {text}"));
    }

    fn notify(&self, plugin: &PluginId, message: String) {
        self.0
            .lock()
            .unwrap()
            .push(format!("notify {plugin} {message}"));
    }
}

fn with_sink() -> (Opts, Arc<Pushed>) {
    let pushed = Arc::new(Pushed::default());
    let mut services = Services::new();
    services.insert(Arc::new(PluginUi::new(
        Arc::clone(&pushed) as Arc<dyn UiSink>
    )));
    (
        Opts {
            services,
            ..Opts::default()
        },
        pushed,
    )
}

#[test]
fn a_panel_renders_a_validated_tree() {
    let Some(fx) = fixtures("ui_render") else {
        return;
    };
    let w = world("ui-render", &fx, "probe", &Opts::default());
    let tree = block_on(&w.rt, w.host.render_panel(&w.id, &cid("tree")))
        .unwrap()
        .unwrap();
    assert_eq!(tree.nodes().len(), 3);
    assert!(matches!(tree.root().widget, Widget::Stack(_)));
    w.done();
}

#[test]
fn a_bad_tree_is_an_error_value_and_not_a_fault() {
    let Some(fx) = fixtures("ui_bad_tree") else {
        return;
    };
    let w = world("ui-bad-tree", &fx, "probe", &Opts::default());
    for _ in 0..4 {
        let r = block_on(&w.rt, w.host.render_panel(&w.id, &cid("bad-tree"))).unwrap();
        assert_eq!(r.unwrap_err(), WidgetError::Empty);
    }
    // Four bad trees in a row would have disabled a faulting plugin (three faults).
    assert_eq!(w.host.plugins()[0].state, PluginState::Live);
    w.command("ok").unwrap();
    w.done();
}

#[test]
fn a_panel_event_can_replace_the_tree() {
    let Some(fx) = fixtures("ui_event") else {
        return;
    };
    let w = world("ui-event", &fx, "probe", &Opts::default());
    let replaced = block_on(
        &w.rt,
        w.host
            .panel_event(&w.id, &cid("tree"), WidgetEvent::Clicked("go".to_owned())),
    )
    .unwrap()
    .expect("the probe answers a click with a tree")
    .unwrap();
    let Widget::Text(text) = &replaced.nodes()[1].widget else {
        panic!("{:?}", replaced.nodes()[1]);
    };
    assert_eq!(text.content, "clicked go");
    let untouched = block_on(
        &w.rt,
        w.host.panel_event(
            &w.id,
            &cid("tree"),
            WidgetEvent::Toggled {
                id: "x".to_owned(),
                checked: true,
            },
        ),
    )
    .unwrap();
    assert!(untouched.is_none());
    w.done();
}

#[test]
fn undeclared_contributions_never_reach_the_guest() {
    let Some(fx) = fixtures("ui_undeclared") else {
        return;
    };
    let w = world("ui-undeclared", &fx, "probe", &Opts::default());
    let r = block_on(&w.rt, w.host.render_panel(&w.id, &cid("nope")));
    assert!(
        matches!(r, Err(CallError::Undeclared { what: "panel" })),
        "{r:?}"
    );
    let r = w.command("nope");
    assert!(
        matches!(r, Err(CallError::Undeclared { what: "command" })),
        "{r:?}"
    );
    let r = block_on(&w.rt, w.host.status(&w.id, &cid("nope")));
    assert!(
        matches!(
            r,
            Err(CallError::Undeclared {
                what: "status item"
            })
        ),
        "{r:?}"
    );
    w.done();
}

#[test]
fn a_panel_without_ui_panel_is_denied() {
    let Some(fx) = fixtures("ui_denied") else {
        return;
    };
    let opts = Opts {
        approve: Approve::Only(&["db.read"]),
        ..Opts::default()
    };
    let w = world("ui-denied", &fx, "probe", &opts);
    let r = block_on(&w.rt, w.host.render_panel(&w.id, &cid("tree")));
    match r {
        Err(CallError::Guest(e)) => assert_eq!(e.code, ErrorCode::CapabilityDenied),
        other => panic!("{other:?}"),
    }
    // A command needs no capability.
    w.command("ok").unwrap();
    w.done();
}

#[test]
fn the_ui_import_is_rate_limited_per_plugin() {
    let Some(fx) = fixtures("ui_limits") else {
        return;
    };
    let (opts, pushed) = with_sink();
    let w = world("ui-limits", &fx, "probe", &opts);
    w.command("ui-calls").unwrap();
    // 100 status texts in a burst: the bucket holds 20 and refills at 10 per second (loose upper bound).
    let status = pushed.count("status ");
    assert!((20..60).contains(&status), "{status} status texts");
    assert_eq!(pushed.count("notify "), 3);
    assert_eq!(pushed.count("refresh probe/tree"), 1);
    w.done();
}

#[test]
fn the_ui_import_without_a_sink_is_dropped_silently() {
    let Some(fx) = fixtures("ui_no_sink") else {
        return;
    };
    let w = world("ui-no-sink", &fx, "probe", &Opts::default());
    w.command("ui-calls").unwrap();
    assert_eq!(w.host.plugins()[0].state, PluginState::Live);
    w.done();
}

#[test]
fn the_command_context_reaches_the_guest() {
    let Some(fx) = fixtures("ui_context") else {
        return;
    };
    let w = world("ui-context", &fx, "probe", &Opts::default());
    let r = w.command_in(
        "context",
        CommandContext::Editor {
            connection: Some("conn-1".to_owned()),
            selection: "select 1".to_owned(),
        },
    );
    match r {
        Err(CallError::Guest(e)) => {
            assert!(e.message.contains("Editor"), "{}", e.message);
            assert!(e.message.contains("conn-1"), "{}", e.message);
            assert!(e.message.contains("select 1"), "{}", e.message);
        }
        other => panic!("{other:?}"),
    }
    w.done();
}
