// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Unit tests that need the crate-internal `run` (the public surface is covered in `tests/runtime_*.rs`).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sqler_plugin_api::{CapabilitySet, GrantKey, Limits, PluginId};
use wasmtime::Trap;

use super::bindings::sqler::plugin::{catalog, query, types};
use super::{CallFn, HostOptions, Live, PluginHost, Services};
use crate::egress::NoEgress;
use crate::error::{CallError, ErrorCode};
use crate::grant::GrantSource;
use crate::settings::MemorySettings;
use crate::status::{CallKind, DisableReason, PluginState};
use crate::{PluginDirs, discover};

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Approves the declared capabilities of every plugin listed.
struct Approve(Mutex<Vec<(GrantKey, CapabilitySet)>>);

impl GrantSource for Approve {
    fn approved(&self, key: &GrantKey) -> Option<CapabilitySet> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, s)| s.clone())
    }
}

fn wat(name: &str) -> Vec<u8> {
    wat::parse_str(match name {
        "ok" => include_str!("../../tests/fixtures/wat/ok.wat"),
        other => panic!("no fixture {other}"),
    })
    .unwrap()
}

struct Fixture {
    _scratch: Scratch,
    rt: tokio::runtime::Runtime,
    host: PluginHost,
}

/// `plugins`: (id, capabilities). Every one runs the `ok` fixture and is approved what it declares.
fn fixture(test: &str, plugins: &[(&str, &[&str])], limits: &Limits) -> Fixture {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    fixture_on(rt, test, plugins, limits)
}

/// Like [`fixture`], on the given runtime.
fn fixture_on(
    rt: tokio::runtime::Runtime,
    test: &str,
    plugins: &[(&str, &[&str])],
    limits: &Limits,
) -> Fixture {
    let root = std::env::temp_dir().join(format!("sqler-host-unit-{}-{test}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let scratch = Scratch(root.clone());
    let mut dirs = Vec::new();
    for (id, caps) in plugins {
        let dir = root.join(id);
        fs::create_dir_all(&dir).unwrap();
        let caps = caps
            .iter()
            .map(|c| format!("\"{c}\""))
            .collect::<Vec<_>>()
            .join(", ");
        fs::write(
            dir.join("sqler.toml"),
            format!("[plugin]\nid = \"{id}\"\nname = \"T\"\nversion = \"1.0.0\"\napi = \"0.1\"\ncapabilities = [{caps}]\n\n[entry]\ncomponent = \"plugin.wasm\"\n"),
        )
        .unwrap();
        fs::write(dir.join("plugin.wasm"), wat("ok")).unwrap();
        dirs.push(dir);
    }
    let d = discover(&PluginDirs {
        dev: dirs,
        ..PluginDirs::default()
    });
    assert!(d.problems.is_empty(), "{:?}", d.problems);
    let grants = Approve(Mutex::new(
        d.plugins
            .iter()
            .map(|p| (p.key.clone(), p.manifest.capabilities().clone()))
            .collect(),
    ));
    let host = PluginHost::new(HostOptions {
        runtime: rt.handle().clone(),
        grants: Arc::new(grants),
        settings: Arc::new(MemorySettings::default()),
        egress: Arc::new(NoEgress),
        limits: *limits,
        services: Services::new(),
    });
    assert_eq!(host.load(d).len(), 0);
    Fixture {
        _scratch: scratch,
        rt,
        host,
    }
}

fn id(s: &str) -> PluginId {
    s.parse().unwrap()
}

fn limits() -> Limits {
    let mut l = Limits::DEFAULT;
    l.deadlines.command = Duration::from_millis(100);
    l
}

fn block_on<F: std::future::Future>(f: &Fixture, fut: F) -> F::Output {
    f.rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(30), fut)
            .await
            .expect("hang guard")
    })
}

fn fails(kind: Trap) -> CallFn<()> {
    Box::new(move |_: &mut Live| Box::pin(async move { Err(kind.into()) }))
}

fn succeeds() -> CallFn<()> {
    Box::new(|_: &mut Live| Box::pin(async { Ok(()) }))
}

fn command(f: &Fixture, who: &str, call: CallFn<()>) -> Result<(), CallError> {
    block_on(f, f.host.run(&id(who), CallKind::Command, call))
}

#[test]
fn unbound_interfaces_answer_by_the_gate() {
    let f = fixture(
        "unbound",
        &[("denied", &[]), ("reader", &["db.read"])],
        &limits(),
    );
    let probe = |who: &str| {
        let call: CallFn<(ErrorCode, ErrorCode, String)> = Box::new(|live: &mut Live| {
            Box::pin(async move {
                let s = live.store.data_mut();
                let c = catalog::Host::connections(s).await.unwrap_err();
                let q = query::Host::run(
                    s,
                    query::QueryRequest {
                        connection: String::new(),
                        sql: String::new(),
                        params: Vec::new(),
                        max_rows: None,
                        timeout_ms: None,
                    },
                )
                .await
                .unwrap_err();
                Ok((c.code.into(), q.code.into(), c.message))
            })
        });
        block_on(&f, f.host.run(&id(who), CallKind::Command, call)).unwrap()
    };
    let (c, q, _) = probe("denied");
    assert_eq!(
        (c, q),
        (ErrorCode::CapabilityDenied, ErrorCode::CapabilityDenied)
    );
    let (c, q, message) = probe("reader");
    assert_eq!((c, q), (ErrorCode::Failed, ErrorCode::Failed));
    assert_eq!(message, "not available in this host");
    f.host.shutdown();
}

#[test]
fn a_fault_drops_only_that_instance() {
    let f = fixture("drop", &[("alpha", &[]), ("beta", &[])], &limits());
    let (a, b) = (id("alpha"), id("beta"));
    block_on(&f, async {
        f.host.init(&a).await.unwrap();
        f.host.init(&b).await.unwrap();
    });
    let e = command(&f, "alpha", fails(Trap::UnreachableCodeReached)).unwrap_err();
    assert!(matches!(e, CallError::Trap { .. }), "{e:?}");
    let states: Vec<_> = f.host.plugins().into_iter().map(|p| p.state).collect();
    assert_eq!(states, [PluginState::Idle, PluginState::Live]);
    // The next call to alpha instantiates (and runs init) again.
    command(&f, "alpha", succeeds()).unwrap();
    assert_eq!(f.host.plugins()[0].state, PluginState::Live);
    f.host.shutdown();
}

#[test]
fn only_consecutive_faults_count() {
    let f = fixture("consecutive", &[("alpha", &[])], &limits());
    for _ in 0..2 {
        command(&f, "alpha", fails(Trap::UnreachableCodeReached)).unwrap_err();
    }
    command(&f, "alpha", succeeds()).unwrap();
    for _ in 0..2 {
        command(&f, "alpha", fails(Trap::UnreachableCodeReached)).unwrap_err();
    }
    assert!(!matches!(
        f.host.plugins()[0].state,
        PluginState::Disabled { .. }
    ));
    command(&f, "alpha", fails(Trap::UnreachableCodeReached)).unwrap_err();
    assert!(matches!(
        f.host.plugins()[0].state,
        PluginState::Disabled {
            reason: DisableReason::Faults { count: 3, .. }
        }
    ));
    f.host.shutdown();
}

#[test]
fn errors_that_are_not_faults_keep_the_instance() {
    let f = fixture("keep", &[("alpha", &[])], &limits());
    let missing: CallFn<()> = Box::new(|_: &mut Live| {
        Box::pin(async {
            Err(CallError::MissingExport {
                interface: "ui-handler",
            }
            .into())
        })
    });
    let e = command(&f, "alpha", missing).unwrap_err();
    assert_eq!(
        e,
        CallError::MissingExport {
            interface: "ui-handler"
        }
    );
    assert_eq!(f.host.plugins()[0].state, PluginState::Live);
    f.host.shutdown();
}

#[test]
fn a_host_panic_is_contained_and_counts_as_a_fault() {
    let f = fixture("panic", &[("alpha", &[]), ("beta", &[])], &limits());
    let boom: CallFn<()> = Box::new(|_: &mut Live| Box::pin(async { panic!("test panic") }));
    let e = command(&f, "alpha", boom).unwrap_err();
    assert_eq!(e, CallError::HostPanic);
    assert!(e.is_fault());
    command(&f, "beta", succeeds()).unwrap();
    // The panicked instance was dropped; alpha instantiates afresh.
    command(&f, "alpha", succeeds()).unwrap();
    f.host.shutdown();
}

#[test]
fn a_call_that_waits_in_the_host_hits_its_deadline() {
    let f = fixture("wait", &[("alpha", &[])], &limits());
    let wait: CallFn<()> = Box::new(|_: &mut Live| {
        Box::pin(async {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            Ok(())
        })
    });
    let e = command(&f, "alpha", wait).unwrap_err();
    assert_eq!(
        e,
        CallError::Timeout {
            after: Duration::from_millis(100)
        }
    );
    assert_eq!(f.host.plugins()[0].state, PluginState::Idle);
    f.host.shutdown();
}

#[test]
fn a_cancelled_call_ends_with_cancelled() {
    let f = fixture("cancel", &[("alpha", &[])], &limits());
    let wait: CallFn<()> = Box::new(|_: &mut Live| {
        Box::pin(async {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            Ok(())
        })
    });
    let e = block_on(&f, async {
        let call = f.host.run(&id("alpha"), CallKind::AiTool, wait);
        tokio::time::sleep(Duration::from_millis(20)).await;
        call.cancel();
        call.await
    })
    .unwrap_err();
    assert_eq!(e, CallError::Cancelled);
    f.host.shutdown();
}

#[test]
fn guest_errors_returned_by_a_call_are_not_faults() {
    let f = fixture("guesterr", &[("alpha", &[])], &limits());
    let err: CallFn<()> = Box::new(|_: &mut Live| {
        Box::pin(async {
            Err(CallError::Guest(crate::error::GuestError {
                code: ErrorCode::Declined,
                message: "no".into(),
            })
            .into())
        })
    });
    let e = command(&f, "alpha", err).unwrap_err();
    assert!(matches!(e, CallError::Guest(_)));
    assert!(!e.is_fault());
    assert_eq!(f.host.plugins()[0].state, PluginState::Live);
    let _: types::ErrorCode = ErrorCode::Failed.into();
    f.host.shutdown();
}

#[test]
fn a_queued_call_cannot_slip_past_the_third_fault() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let f = fixture("queued-faults", &[("alpha", &[])], &limits());
    let ran = Arc::new(AtomicUsize::new(0));
    let calls: Vec<_> = (0..6)
        .map(|_| {
            let ran = Arc::clone(&ran);
            let call: CallFn<()> = Box::new(move |_: &mut Live| {
                Box::pin(async move {
                    ran.fetch_add(1, Ordering::SeqCst);
                    Err(Trap::UnreachableCodeReached.into())
                })
            });
            // All six are queued behind the plugin lock before any of them has run.
            f.host.run(&id("alpha"), CallKind::Command, call)
        })
        .collect();
    let results = block_on(&f, async {
        let mut out = Vec::new();
        for c in calls {
            out.push(c.await);
        }
        out
    });
    assert_eq!(ran.load(Ordering::SeqCst), 3, "{results:?}");
    for r in &results[..3] {
        assert!(matches!(r, Err(CallError::Trap { .. })), "{r:?}");
    }
    for r in &results[3..] {
        assert_eq!(r, &Err(CallError::Disabled));
    }
    f.host.shutdown();
}

#[test]
fn an_exit_does_not_refill_the_log_bucket() {
    use std::time::Instant;

    use crate::log::Admit;

    let f = fixture("exit-log", &[("alpha", &[])], &limits());
    let entry = f.host.inner.entry(&id("alpha")).unwrap();
    command(&f, "alpha", succeeds()).unwrap();
    let drain = |n: usize| {
        let mut l = entry.log.lock().unwrap();
        (0..n)
            .filter(|_| matches!(l.admit(Instant::now()), Admit::Pass { .. }))
            .count()
    };
    assert!(drain(150) >= 90, "the bucket starts full");
    let exit: CallFn<()> =
        Box::new(|_: &mut Live| Box::pin(async { Err(wasmtime_wasi::I32Exit(3).into()) }));
    let e = command(&f, "alpha", exit).unwrap_err();
    assert!(
        matches!(&e, CallError::Trap { message } if message.contains("status 3")),
        "{e:?}"
    );
    assert!(e.is_fault());
    // The next call instantiates again, which logs a line through the same bucket.
    command(&f, "alpha", succeeds()).unwrap();
    // A fresh bucket would pass about 100 lines; the old one has only what 20 lines/s refilled meanwhile.
    assert!(drain(150) < 10);
    f.host.shutdown();
}

#[test]
fn the_wasi_resource_table_is_capped() {
    let f = fixture("resources", &[("alpha", &[])], &limits());
    let probe: CallFn<usize> = Box::new(|live: &mut Live| {
        Box::pin(async move {
            let table = live.store.data_mut().resources();
            let mut pushed = 0;
            while table.push(pushed).is_ok() {
                pushed += 1;
            }
            Ok(pushed)
        })
    });
    let pushed = block_on(&f, f.host.run(&id("alpha"), CallKind::Command, probe)).unwrap();
    assert_eq!(pushed, super::state::MAX_RESOURCES);
    f.host.shutdown();
}

#[test]
fn wait_blocking_returns_to_a_plain_thread_while_the_host_runtime_runs_the_call() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let f = fixture_on(rt, "wait-blocking", &[("alpha", &[])], &limits());
    // This test thread is not a thread of the host's runtime.
    let ok = f
        .host
        .wait_blocking(f.host.run(&id("alpha"), CallKind::Command, succeeds()));
    assert_eq!(ok, Ok(()));
    let bad = f.host.wait_blocking(f.host.run(
        &id("alpha"),
        CallKind::Command,
        fails(Trap::UnreachableCodeReached),
    ));
    assert!(matches!(bad, Err(CallError::Trap { .. })), "{bad:?}");
    let missing = f
        .host
        .wait_blocking(f.host.run(&id("nobody"), CallKind::Command, succeeds()));
    assert_eq!(missing, Err(CallError::NotFound));
    f.host.shutdown();
    let after = f
        .host
        .wait_blocking(f.host.run(&id("alpha"), CallKind::Command, succeeds()));
    assert!(after.is_err(), "{after:?}");
}
