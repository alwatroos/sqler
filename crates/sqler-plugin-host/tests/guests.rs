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

use std::convert::Infallible;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use sqler_plugin_api::{Capability, CapabilitySet, PluginId, SettingValue};
use sqler_plugin_host::{
    CallError, EgressFactory, ErrorCode, GrantSource, HostOptions, MemorySettings, PluginDirs,
    PluginHost, PluginState, Services, discover,
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
    let complete = resolved
        .as_ref()
        .filter(|d| d.join("probe/probe.wasm").is_file() && d.join("smoke/smoke.wasm").is_file());
    if let Some(d) = complete {
        return Some(d.clone());
    }
    assert!(
        !required,
        "{test}: SQLER_TEST_REQUIRE_PLUGINS=1 but SQLER_PLUGIN_FIXTURES is unset or lacks the built probe and smoke \
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
}

impl Default for Opts {
    fn default() -> Self {
        Self {
            approve: Approve::Declared,
            server: false,
            command: Duration::from_secs(30),
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
    let id = plugin.manifest.id.clone();
    let grants = Arc::new(Grants::default());
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
    limits.deadlines.status = Duration::from_secs(30);
    limits.deadlines.command = opts.command;
    let host = PluginHost::new(HostOptions {
        runtime: rt.handle().clone(),
        grants: grants as Arc<dyn GrantSource>,
        settings: Arc::new(MemorySettings::default()),
        egress: egress(),
        limits,
        services: Services::new(),
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
    }
}

impl World {
    fn status(&self, item: &str) -> String {
        block_on(&self.rt, self.host.call_status(&self.id, item))
            .unwrap()
            .unwrap_or_else(|| panic!("no status for {item}"))
    }

    fn command(&self, command: &str) -> Result<(), CallError> {
        block_on(&self.rt, self.host.call_command(&self.id, command))
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
    // An unknown command is a guest error, not a fault.
    match w.command("nope") {
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
    assert_eq!(w.status("setting"), "");
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
