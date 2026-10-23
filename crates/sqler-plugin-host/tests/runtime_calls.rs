// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The runtime through its public surface, with hand-written WAT components. Every `init` does the interesting thing.

#![cfg(feature = "cl")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use sqler_plugin_api::PluginId;
use sqler_plugin_host::{CallError, DisableReason, LoadError, PluginHost, PluginState};

use common::{
    Grants, Scratch, block_on, capture, discover_all, host, plugin_lines, runtime, test_limits,
    write_plugin,
};

fn id(s: &str) -> PluginId {
    s.parse().unwrap()
}

/// A host with the given fixture plugins (id, wat), all approved.
struct Setup {
    _scratch: Scratch,
    rt: tokio::runtime::Runtime,
    host: PluginHost,
}

fn setup(test: &str, plugins: &[(&str, &str)], init: Duration) -> Setup {
    let scratch = Scratch::new(test);
    let dirs: Vec<_> = plugins
        .iter()
        .map(|(i, wat)| write_plugin(scratch.path(), i, wat, &[]))
        .collect();
    let d = discover_all(&dirs);
    let grants = Arc::new(Grants::default());
    grants.approve_declared(&d);
    let rt = runtime();
    let host = host(&rt, grants, &test_limits(init));
    assert_eq!(host.load(d).len(), 0);
    Setup {
        _scratch: scratch,
        rt,
        host,
    }
}

const SHORT: Duration = Duration::from_millis(100);
const LONG: Duration = Duration::from_secs(30);

#[test]
fn new_and_load_do_no_work() {
    let s = setup("idle", &[("alpha", "ok")], SHORT);
    assert!(s.host.is_idle());
    let status = s.host.plugins();
    assert_eq!(status.len(), 1);
    assert_eq!(status[0].state, PluginState::Idle);
    s.host.shutdown();
    assert!(
        s.host.is_idle(),
        "shutdown of an unused host must not create an engine"
    );
}

#[test]
fn ok_init_runs_once_and_logs_through_the_host() {
    let s = setup("ok", &[("alpha", "ok")], LONG);
    let a = id("alpha");
    let ((), lines) = capture(|| {
        block_on(&s.rt, async {
            s.host.init(&a).await.unwrap();
            // Already live: no second init.
            s.host.init(&a).await.unwrap();
        });
    });
    let logged = plugin_lines(&lines);
    assert_eq!(logged.len(), 1, "{lines:?}");
    assert_eq!(logged[0].message, "init");
    assert_eq!(logged[0].plugin.as_deref(), Some("alpha"));
    assert_eq!(logged[0].level, tracing::Level::INFO);
    assert!(!s.host.is_idle());
    assert_eq!(s.host.plugins()[0].state, PluginState::Live);
    s.host.shutdown();
}

#[test]
fn refresh_grants_drops_the_instance_so_init_runs_again() {
    let s = setup("refresh", &[("alpha", "ok")], LONG);
    let a = id("alpha");
    let ((), lines) = capture(|| {
        block_on(&s.rt, async {
            s.host.init(&a).await.unwrap();
            s.host.refresh_grants(&a);
            assert_eq!(s.host.plugins()[0].state, PluginState::Idle);
            s.host.init(&a).await.unwrap();
        });
    });
    assert_eq!(plugin_lines(&lines).len(), 2, "{lines:?}");
    s.host.shutdown();
}

#[test]
fn spin_times_out_at_the_deadline() {
    let s = setup("spin", &[("alpha", "spin")], SHORT);
    let e = block_on(&s.rt, s.host.init(&id("alpha"))).unwrap_err();
    assert_eq!(e, CallError::Timeout { after: SHORT });
    assert!(e.is_fault());
    assert_eq!(s.host.plugins()[0].state, PluginState::Idle);
    s.host.shutdown();
}

#[test]
fn trap_is_a_trap() {
    let s = setup("trap", &[("alpha", "trap")], LONG);
    let e = block_on(&s.rt, s.host.init(&id("alpha"))).unwrap_err();
    assert!(matches!(e, CallError::Trap { .. }), "{e:?}");
    s.host.shutdown();
}

#[test]
fn growth_past_the_cap_is_a_memory_limit() {
    let s = setup("grow", &[("alpha", "grow")], LONG);
    let e = block_on(&s.rt, s.host.init(&id("alpha"))).unwrap_err();
    assert_eq!(
        e,
        CallError::MemoryLimit {
            limit: sqler_plugin_api::Limits::DEFAULT.memory_bytes
        }
    );
    s.host.shutdown();
}

#[test]
fn memories_count_together_against_the_cap() {
    // Two memories of 37.5 MiB each: either alone fits the 64 MiB cap.
    let s = setup("two-memories", &[("alpha", "two-memories")], LONG);
    let e = block_on(&s.rt, s.host.init(&id("alpha"))).unwrap_err();
    assert_eq!(
        e,
        CallError::MemoryLimit {
            limit: sqler_plugin_api::Limits::DEFAULT.memory_bytes
        }
    );
    assert!(e.is_fault());
    s.host.shutdown();
}

/// A module using a proposal the engine pins off must not even compile, so it never reaches a limiter.
fn assert_refused_at_compile(fixture: &str) {
    let s = setup(fixture, &[("alpha", fixture)], LONG);
    let e = block_on(&s.rt, s.host.init(&id("alpha"))).unwrap_err();
    let CallError::Load(LoadError::Compile { message }) = &e else {
        panic!("{e:?}")
    };
    println!("{message}");
    assert!(!e.is_fault());
    assert!(matches!(
        s.host.plugins()[0].state,
        PluginState::Disabled {
            reason: DisableReason::Load(_)
        }
    ));
    s.host.shutdown();
}

#[test]
fn a_shared_memory_is_refused_because_the_limiter_cannot_see_it() {
    assert_refused_at_compile("shared-memory");
}

#[test]
fn a_64_bit_memory_is_refused() {
    assert_refused_at_compile("memory64");
}

#[test]
fn tables_count_together_against_their_cap() {
    // Sixty tables of 20000 elements: 1.2 million together, 1 million at most.
    let s = setup("many-tables", &[("alpha", "many-tables")], LONG);
    let e = block_on(&s.rt, s.host.init(&id("alpha"))).unwrap_err();
    assert!(matches!(e, CallError::MemoryLimit { .. }), "{e:?}");
    s.host.shutdown();
}

#[test]
fn a_guest_that_exits_is_a_fault_and_is_disabled_after_three() {
    let s = setup("exit", &[("alpha", "exit")], LONG);
    let a = id("alpha");
    block_on(&s.rt, async {
        for _ in 0..3 {
            let e = s.host.init(&a).await.unwrap_err();
            let CallError::Trap { message } = &e else {
                panic!("{e:?}")
            };
            assert!(message.contains("guest exited (status 1)"), "{message}");
            assert!(e.is_fault());
        }
        assert_eq!(s.host.init(&a).await.unwrap_err(), CallError::Disabled);
    });
    assert!(matches!(
        s.host.plugins()[0].state,
        PluginState::Disabled {
            reason: DisableReason::Faults { count: 3, .. }
        }
    ));
    s.host.shutdown();
}

/// Runs `f` on its own thread and fails the test when it has not returned after `secs`: a wedged runtime worker would
/// also stall a timeout that runs on it.
fn hang_guard(secs: u64, f: impl FnOnce() + Send + 'static) {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        f();
        let _ = tx.send(());
    });
    rx.recv_timeout(Duration::from_secs(secs))
        .expect("hang guard: a call racing with shutdown did not end");
}

fn threaded() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

fn spin_host(test: &str, scratch: &Scratch, rt: &tokio::runtime::Runtime) -> PluginHost {
    let dir = write_plugin(scratch.path(), "alpha", "spin", &[]);
    let d = discover_all(&[dir]);
    let grants = Arc::new(Grants::default());
    grants.approve_declared(&d);
    let host = host(rt, grants, &test_limits(LONG));
    assert_eq!(host.load(d).len(), 0, "{test}");
    host
}

#[test]
fn a_call_racing_with_shutdown_ends_and_never_wedges() {
    hang_guard(120, || {
        for i in 0..24u64 {
            let scratch = Scratch::new(&format!("race-{i}"));
            let rt = threaded();
            let host = spin_host("race", &scratch, &rt);
            let call = host.init(&id("alpha"));
            // Spread the shutdown over the phases of the call: before the task starts, while it prepares, while it spins.
            std::thread::sleep(Duration::from_micros(i * 700));
            host.shutdown();
            let e = rt
                .block_on(call)
                .expect_err("a spinning init cannot succeed");
            assert!(
                matches!(e, CallError::Cancelled | CallError::Engine { .. }),
                "iteration {i}: {e:?}"
            );
            // Later calls are refused, and the host does not start an engine after the stop.
            let e = rt.block_on(host.init(&id("alpha"))).unwrap_err();
            assert!(matches!(e, CallError::Engine { .. }), "{e:?}");
        }
    });
}

#[test]
fn shutdown_ends_a_call_that_waits_for_the_plugin() {
    hang_guard(60, || {
        let scratch = Scratch::new("queued");
        let rt = threaded();
        let host = spin_host("queued", &scratch, &rt);
        let a = id("alpha");
        let first = host.init(&a);
        let second = host.init(&a);
        std::thread::sleep(Duration::from_millis(150));
        host.shutdown();
        for call in [first, second] {
            let e = rt.block_on(call).unwrap_err();
            assert!(
                matches!(e, CallError::Cancelled | CallError::Engine { .. }),
                "{e:?}"
            );
        }
    });
}

#[test]
fn unknown_import_is_a_link_error_and_disables_with_the_load_reason() {
    let s = setup("unknown", &[("alpha", "unknown-import")], LONG);
    let a = id("alpha");
    let e = block_on(&s.rt, s.host.init(&a)).unwrap_err();
    let CallError::Load(load) = &e else {
        panic!("{e:?}")
    };
    assert!(matches!(load, LoadError::Link { .. }), "{load:?}");
    assert!(load.is_incompatible());
    assert!(!e.is_fault());
    assert!(matches!(
        s.host.plugins()[0].state,
        PluginState::Disabled {
            reason: DisableReason::Load(_)
        }
    ));
    // Disabled for a load reason: the same error, without another attempt.
    assert_eq!(block_on(&s.rt, s.host.init(&a)).unwrap_err(), e);
    s.host.shutdown();
}

#[test]
fn a_component_without_lifecycle_is_a_missing_export() {
    let s = setup("nolc", &[("alpha", "no-lifecycle")], LONG);
    let e = block_on(&s.rt, s.host.init(&id("alpha"))).unwrap_err();
    assert_eq!(
        e,
        CallError::MissingExport {
            interface: "lifecycle"
        }
    );
    assert!(!e.is_fault());
    s.host.shutdown();
}

#[test]
fn cancel_ends_a_spinning_call() {
    let s = setup("cancel", &[("alpha", "spin")], LONG);
    let e = block_on(&s.rt, async {
        let call = s.host.init(&id("alpha"));
        // Let the call start and spin; the outcome is Cancelled whether or not it did.
        tokio::time::sleep(Duration::from_millis(50)).await;
        call.cancel();
        call.await
    })
    .unwrap_err();
    assert_eq!(e, CallError::Cancelled);
    assert_eq!(s.host.plugins()[0].state, PluginState::Idle);
    s.host.shutdown();
}

#[test]
fn refresh_grants_cancels_a_call_in_flight() {
    // A revoked capability must not keep working until the call's deadline (30 s for commands).
    let s = setup("refresh-cancel", &[("alpha", "spin")], LONG);
    let a = id("alpha");
    let started = std::time::Instant::now();
    let e = block_on(&s.rt, async {
        let call = s.host.init(&a);
        tokio::time::sleep(Duration::from_millis(50)).await;
        s.host.refresh_grants(&a);
        call.await
    })
    .unwrap_err();
    assert_eq!(e, CallError::Cancelled);
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(!e.is_fault());
    s.host.shutdown();
}

#[cfg(unix)]
#[test]
fn a_data_root_that_leaves_the_plugin_is_reported_in_the_status() {
    let scratch = Scratch::new("data-link");
    let dir = write_plugin(scratch.path(), "alpha", "ok", &["fs.read:data"]);
    let elsewhere = scratch.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, dir.join("data")).unwrap();
    let d = discover_all(&[dir]);
    let grants = Arc::new(Grants::default());
    grants.approve_declared(&d);
    let rt = runtime();
    let host = host(&rt, grants, &test_limits(LONG));
    assert_eq!(host.load(d).len(), 0);
    assert_eq!(host.plugins()[0].scope_issues, []);
    block_on(&rt, host.init(&id("alpha"))).unwrap();
    let issues = host.plugins()[0].scope_issues.clone();
    assert_eq!(issues.len(), 1, "{issues:?}");
    assert!(issues[0].to_string().contains("fs.read:data"));
    host.shutdown();
}

#[test]
fn a_spinning_plugin_does_not_hold_up_another() {
    let s = setup("parallel", &[("alpha", "spin"), ("beta", "ok")], LONG);
    block_on(&s.rt, async {
        let a = s.host.init(&id("alpha"));
        tokio::time::sleep(Duration::from_millis(50)).await;
        s.host.init(&id("beta")).await.unwrap();
        a.cancel();
        assert_eq!(a.await.unwrap_err(), CallError::Cancelled);
    });
    s.host.shutdown();
}

#[test]
fn three_consecutive_faults_disable_and_enable_restores() {
    let s = setup("faults", &[("alpha", "trap")], LONG);
    let a = id("alpha");
    block_on(&s.rt, async {
        for _ in 0..3 {
            assert!(matches!(
                s.host.init(&a).await.unwrap_err(),
                CallError::Trap { .. }
            ));
        }
        assert_eq!(s.host.init(&a).await.unwrap_err(), CallError::Disabled);
        match &s.host.plugins()[0].state {
            PluginState::Disabled {
                reason: DisableReason::Faults { count, last },
            } => {
                assert_eq!(*count, 3);
                assert!(matches!(last, CallError::Trap { .. }));
            }
            other => panic!("{other:?}"),
        }
        s.host.enable(&a);
        assert_eq!(s.host.plugins()[0].state, PluginState::Idle);
        assert!(matches!(
            s.host.init(&a).await.unwrap_err(),
            CallError::Trap { .. }
        ));
    });
    s.host.shutdown();
}

#[test]
fn disable_unload_and_unknown_ids() {
    let s = setup("verbs", &[("alpha", "ok")], LONG);
    let a = id("alpha");
    block_on(&s.rt, async {
        s.host.init(&a).await.unwrap();
        s.host.disable(&a);
        assert_eq!(s.host.init(&a).await.unwrap_err(), CallError::Disabled);
        assert!(matches!(
            s.host.plugins()[0].state,
            PluginState::Disabled {
                reason: DisableReason::User
            }
        ));
        s.host.enable(&a);
        s.host.init(&a).await.unwrap();
        s.host.unload(&a);
        assert_eq!(s.host.plugins().len(), 0);
        assert_eq!(s.host.init(&a).await.unwrap_err(), CallError::NotFound);
        assert_eq!(
            s.host.init(&id("zzz")).await.unwrap_err(),
            CallError::NotFound
        );
    });
    s.host.shutdown();
}

#[test]
fn calls_after_shutdown_fail() {
    let s = setup("shutdown", &[("alpha", "ok")], LONG);
    block_on(&s.rt, s.host.init(&id("alpha"))).unwrap();
    s.host.shutdown();
    let e = block_on(&s.rt, s.host.init(&id("alpha"))).unwrap_err();
    assert!(matches!(e, CallError::Engine { .. }), "{e:?}");
    // Idempotent.
    s.host.shutdown();
}

#[test]
fn status_effective_is_declared_and_approved() {
    let scratch = Scratch::new("effective");
    let dir = write_plugin(scratch.path(), "alpha", "ok", &["db.read", "ui.panel"]);
    let d = discover_all(&[dir]);
    let key = d.plugins[0].key.clone();
    let grants = Arc::new(Grants::default());
    let rt = runtime();
    let host = host(&rt, grants.clone(), &test_limits(LONG));
    assert_eq!(host.load(d).len(), 0);
    assert!(
        host.plugins()[0].effective.is_empty(),
        "no approval, nothing effective"
    );
    grants.approve(
        &key,
        "db.read"
            .parse::<sqler_plugin_api::Capability>()
            .into_iter()
            .collect(),
    );
    let eff = host.plugins()[0].effective.clone();
    assert_eq!(eff.len(), 1);
    assert!(eff.allows(sqler_plugin_api::CapabilityKind::DbRead));
}

#[test]
fn typed_ui_calls_refuse_what_the_manifest_does_not_declare() {
    // The check runs before any guest code, so the component is never instantiated.
    let s = setup("undeclared", &[("alpha", "ok")], LONG);
    let a = id("alpha");
    let thing: sqler_plugin_api::ContributionId = "thing".parse().unwrap();
    let r = block_on(
        &s.rt,
        s.host
            .run_command(&a, &thing, sqler_plugin_api::CommandContext::None),
    );
    assert_eq!(r, Err(CallError::Undeclared { what: "command" }));
    let r = block_on(&s.rt, s.host.render_panel(&a, &thing));
    assert_eq!(r, Err(CallError::Undeclared { what: "panel" }));
    let event = sqler_plugin_api::WidgetEvent::Clicked("x".to_owned());
    let r = block_on(&s.rt, s.host.panel_event(&a, &thing, event));
    assert_eq!(r, Err(CallError::Undeclared { what: "panel" }));
    let r = block_on(&s.rt, s.host.status(&a, &thing));
    assert_eq!(
        r,
        Err(CallError::Undeclared {
            what: "status item"
        })
    );
    let r = block_on(&s.rt, s.host.status(&id("ghost"), &thing));
    assert_eq!(r, Err(CallError::NotFound));
    assert!(s.host.is_idle(), "a refused call must not create an engine");
    s.host.shutdown();
}
