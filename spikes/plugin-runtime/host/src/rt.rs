// SPDX-License-Identifier: Apache-2.0
//! The wasmtime side: engine setup, fixture run, bench, precompile, `wasi:cli/run`.

use std::fmt::Display;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use wasmtime::component::{Component, Instance, InstancePre, Linker, ResourceTable};
use wasmtime::{Config, Engine, Store};
use wasmtime_wasi::p2::bindings::sync::Command;
use wasmtime_wasi::{FsPerms, I32Exit, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use crate::cli::RunCommand;
use crate::report::{emit, ms, overhead_pct, percentile};
use crate::rss_kib;

/// Iterations of the `spin` call used for the fuel and epoch cost.
const SPIN_ITERS: u64 = 20_000_000;
/// Repetitions of each timed `spin`; the median is reported.
const SPIN_REPEATS: usize = 5;
/// An unbounded `spin` must trap within this long.
const INTERRUPT_LIMIT: Duration = Duration::from_secs(1);
/// Epoch tick period of the ticker thread.
const TICK: Duration = Duration::from_millis(10);

fn s<E: Display>(e: E) -> String {
    e.to_string()
}

struct State {
    ctx: WasiCtx,
    table: ResourceTable,
}

impl WasiView for State {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.ctx,
            table: &mut self.table,
        }
    }
}

impl State {
    fn new() -> Self {
        Self {
            ctx: WasiCtxBuilder::new().build(),
            table: ResourceTable::new(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Plain,
    Fuel,
    Epoch,
}

#[allow(clippy::unnecessary_wraps)] // fallible only when a Pulley target is configured
fn config(mode: Mode) -> Result<Config, String> {
    let mut c = Config::new();
    #[cfg(feature = "pulley-target")]
    if cfg!(any(feature = "pulley", feature = "pulley-rt")) {
        c.target("pulley64").map_err(s)?;
    }
    #[cfg(feature = "winch")]
    if crate::variant::NAME == "winch" {
        c.strategy(wasmtime::Strategy::Winch);
    }
    match mode {
        Mode::Plain => {}
        Mode::Fuel => {
            c.consume_fuel(true);
        }
        Mode::Epoch => {
            c.epoch_interruption(true);
        }
    }
    Ok(c)
}

fn engine(mode: Mode) -> Result<Engine, String> {
    Engine::new(&config(mode)?).map_err(s)
}

fn is_cwasm(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "cwasm")
}

/// Time taken by the two ways of getting a [`Component`].
#[derive(Default)]
struct LoadTimes {
    compile: Option<Duration>,
    load_cwasm: Option<Duration>,
}

#[allow(unsafe_code)]
fn load(engine: &Engine, path: &Path) -> Result<(Component, LoadTimes), String> {
    let bytes = fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let mut times = LoadTimes::default();
    let start = Instant::now();
    let component = if is_cwasm(path) {
        // SAFETY: the file was produced by `precompile` of this same binary version and
        // engine config (it is a build artefact of the spike, not untrusted input).
        let c = unsafe { Component::deserialize(engine, &bytes) }.map_err(s)?;
        times.load_cwasm = Some(start.elapsed());
        c
    } else {
        let c = compile(engine, &bytes)?;
        times.compile = Some(start.elapsed());
        c
    };
    Ok((component, times))
}

#[cfg(feature = "compiler")]
fn compile(engine: &Engine, bytes: &[u8]) -> Result<Component, String> {
    Component::new(engine, bytes).map_err(compile_err)
}

#[cfg(not(feature = "compiler"))]
fn compile(_engine: &Engine, _bytes: &[u8]) -> Result<Component, String> {
    Err("this variant has no compiler: pass a .cwasm".to_owned())
}

#[cfg(feature = "compiler")]
fn compile_err<E: Display>(e: E) -> String {
    format!("compile: {e}")
}

fn linker(engine: &Engine) -> Result<Linker<State>, String> {
    let mut l = Linker::<State>::new(engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut l).map_err(s)?;
    Ok(l)
}

fn add(store: &mut Store<State>, instance: &Instance, a: u32, b: u32) -> Result<u32, String> {
    let f = instance
        .get_typed_func::<(u32, u32), (u32,)>(&mut *store, "add")
        .map_err(s)?;
    f.call(&mut *store, (a, b)).map(|r| r.0).map_err(s)
}

fn instantiate(
    engine: &Engine,
    pre: &InstancePre<State>,
) -> Result<(Store<State>, Instance), String> {
    instantiate_with(engine, pre, |_| Ok(()))
}

/// Like [`instantiate`], with a hook that runs on the fresh store before instantiation
/// (fuel must be set first: the component's start code already consumes it).
fn instantiate_with(
    engine: &Engine,
    pre: &InstancePre<State>,
    prepare: impl FnOnce(&mut Store<State>) -> Result<(), String>,
) -> Result<(Store<State>, Instance), String> {
    let mut store = Store::new(engine, State::new());
    prepare(&mut store)?;
    let instance = pre.instantiate(&mut store).map_err(s)?;
    Ok((store, instance))
}

pub fn run_fixture(path: &Path) -> Result<(), String> {
    let engine = engine(Mode::Plain)?;
    let (component, _) = load(&engine, path)?;
    let pre = linker(&engine)?.instantiate_pre(&component).map_err(s)?;
    let (mut store, instance) = instantiate(&engine, &pre)?;
    let sum = add(&mut store, &instance, 2, 3)?;
    emit("fixture_add", sum);
    if sum == 5 {
        Ok(())
    } else {
        Err(format!("add(2,3) returned {sum}"))
    }
}

pub fn precompile(input: &Path, output: &Path, pulley: bool) -> Result<(), String> {
    if !cfg!(feature = "compiler") {
        return Err("precompile needs a compiler; this variant has none".to_owned());
    }
    #[allow(unused_mut)]
    let mut c = config(Mode::Plain)?;
    if pulley {
        #[cfg(feature = "pulley-target")]
        c.target("pulley64").map_err(s)?;
        #[cfg(not(feature = "pulley-target"))]
        return Err("--pulley needs the pulley variant".to_owned());
    }
    let engine = Engine::new(&c).map_err(s)?;
    let bytes = fs::read(input).map_err(|e| format!("read {}: {e}", input.display()))?;
    write_precompiled(&engine, &bytes, output)
}

#[cfg(feature = "compiler")]
fn write_precompiled(engine: &Engine, bytes: &[u8], output: &Path) -> Result<(), String> {
    let out = engine.precompile_component(bytes).map_err(compile_err)?;
    fs::write(output, out).map_err(|e| format!("write {}: {e}", output.display()))
}

#[cfg(not(feature = "compiler"))]
fn write_precompiled(_engine: &Engine, _bytes: &[u8], _output: &Path) -> Result<(), String> {
    Err("precompile needs a compiler; this variant has none".to_owned())
}

fn micros(d: Duration) -> u64 {
    u64::try_from(d.as_micros()).unwrap_or(u64::MAX)
}

fn nanos(d: Duration) -> u64 {
    u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)
}

fn put_pct(name: &str, unit: &str, sorted: &[u64]) {
    emit(
        &format!("{name}_p50_{unit}"),
        percentile(sorted, 50).map_or("n/a".to_owned(), |v| v.to_string()),
    );
    emit(
        &format!("{name}_p95_{unit}"),
        percentile(sorted, 95).map_or("n/a".to_owned(), |v| v.to_string()),
    );
}

#[allow(clippy::cast_precision_loss)]
pub fn bench(path: &Path, instances: usize, samples: usize, calls: usize) -> Result<(), String> {
    let engine = engine(Mode::Plain)?;
    let (component, times) = load(&engine, path)?;
    emit("compile_ms", times.compile.map_or("n/a".to_owned(), ms));
    emit(
        "load_cwasm_ms",
        times.load_cwasm.map_or("n/a".to_owned(), ms),
    );
    let pre = linker(&engine)?.instantiate_pre(&component).map_err(s)?;

    // Instantiate: Store + WasiCtx + instantiate (from an InstancePre made once).
    let mut inst = Vec::with_capacity(samples);
    for _ in 0..samples {
        let t = Instant::now();
        let pair = instantiate(&engine, &pre)?;
        inst.push(micros(t.elapsed()));
        drop(pair);
    }
    inst.sort_unstable();
    put_pct("instantiate", "us", &inst);

    // Call: `add` on one instance.
    let (mut store, instance) = instantiate(&engine, &pre)?;
    let f = instance
        .get_typed_func::<(u32, u32), (u32,)>(&mut store, "add")
        .map_err(s)?;
    let mut call = Vec::with_capacity(calls);
    for i in 0..calls {
        let a = u32::try_from(i % 1000).unwrap_or(0);
        let t = Instant::now();
        let r = f.call(&mut store, (a, 1)).map_err(s)?;
        call.push(nanos(t.elapsed()));
        if r.0 != a + 1 {
            return Err(format!("add({a},1) returned {}", r.0));
        }
    }
    call.sort_unstable();
    put_pct("call", "ns", &call);
    drop(store);

    // Idle memory: RSS delta over `instances` live Stores.
    let before = rss_kib();
    let mut live = Vec::with_capacity(instances);
    for _ in 0..instances {
        live.push(instantiate(&engine, &pre)?);
    }
    let after = rss_kib();
    let idle = match (before, after) {
        (Some(b), Some(a)) => format!("{:.1}", a.saturating_sub(b) as f64 / instances as f64),
        _ => "n/a".to_owned(),
    };
    emit("idle_instance_kib", idle);
    drop(live);

    fuel_epoch(path)
}

/// Median of the repeated timings of `spin(SPIN_ITERS)`.
fn time_spin(store: &mut Store<State>, instance: &Instance) -> Result<Duration, String> {
    let f = instance
        .get_typed_func::<(u64,), (u64,)>(&mut *store, "spin")
        .map_err(s)?;
    let mut runs = Vec::with_capacity(SPIN_REPEATS);
    for _ in 0..SPIN_REPEATS {
        let t = Instant::now();
        f.call(&mut *store, (SPIN_ITERS,)).map_err(s)?;
        runs.push(t.elapsed());
    }
    runs.sort_unstable();
    Ok(runs.get(runs.len() / 2).copied().unwrap_or_default())
}

/// Runs `spin(u64::MAX)` and reports whether it trapped within [`INTERRUPT_LIMIT`].
fn interrupted(store: &mut Store<State>, instance: &Instance) -> Result<bool, String> {
    let f = instance
        .get_typed_func::<(u64,), (u64,)>(&mut *store, "spin")
        .map_err(s)?;
    let t = Instant::now();
    let r = f.call(&mut *store, (u64::MAX,));
    Ok(r.is_err() && t.elapsed() < INTERRUPT_LIMIT)
}

struct Ticker {
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Ticker {
    fn start(engine: &Engine) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let (e, flag) = (engine.clone(), Arc::clone(&stop));
        let handle = thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                thread::sleep(TICK);
                e.increment_epoch();
            }
        });
        Self {
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for Ticker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            // A panicked ticker only means no more ticks; nothing to recover.
            let _ = h.join();
        }
    }
}

struct SpinResult {
    time: Duration,
    interrupt_ok: bool,
}

/// One engine in `mode`: time `spin`, then check an unbounded `spin` is interrupted.
fn spin_in_mode(path: &Path, mode: Mode) -> Result<SpinResult, String> {
    let engine = engine(mode)?;
    let (component, _) = load(&engine, path)?;
    let pre = linker(&engine)?.instantiate_pre(&component).map_err(s)?;
    let _ticker = (mode == Mode::Epoch).then(|| Ticker::start(&engine));

    let arm = |store: &mut Store<State>, budget: bool| -> Result<(), String> {
        match mode {
            Mode::Plain => {}
            Mode::Fuel => store
                .set_fuel(if budget { 5_000_000 } else { 1 << 50 })
                .map_err(s)?,
            Mode::Epoch => {
                store.epoch_deadline_trap();
                store.set_epoch_deadline(if budget { 5 } else { 1 << 40 });
            }
        }
        Ok(())
    };

    let (mut store, instance) = instantiate_with(&engine, &pre, |st| arm(st, false))?;
    let time = time_spin(&mut store, &instance)?;
    let interrupt_ok = if mode == Mode::Plain {
        false
    } else {
        let (mut store, instance) = instantiate_with(&engine, &pre, |st| arm(st, true))?;
        interrupted(&mut store, &instance)?
    };
    Ok(SpinResult { time, interrupt_ok })
}

fn fuel_epoch(path: &Path) -> Result<(), String> {
    if is_cwasm(path) {
        // A precompiled artefact is bound to one engine config; fuel/epoch need their own.
        for k in [
            "spin_none_ms",
            "spin_fuel_ms",
            "spin_epoch_ms",
            "fuel_overhead_pct",
            "epoch_overhead_pct",
            "fuel_interrupt_ok",
            "epoch_interrupt_ok",
        ] {
            emit(k, "n/a");
        }
        return Ok(());
    }
    let none = spin_in_mode(path, Mode::Plain)?;
    let fuel = spin_in_mode(path, Mode::Fuel)?;
    let epoch = spin_in_mode(path, Mode::Epoch)?;
    emit("spin_none_ms", ms(none.time));
    emit("spin_fuel_ms", ms(fuel.time));
    emit("spin_epoch_ms", ms(epoch.time));
    emit("fuel_overhead_pct", overhead_pct(none.time, fuel.time));
    emit("epoch_overhead_pct", overhead_pct(none.time, epoch.time));
    emit("fuel_interrupt_ok", u8::from(fuel.interrupt_ok));
    emit("epoch_interrupt_ok", u8::from(epoch.interrupt_ok));
    Ok(())
}

/// Runs `wasi:cli/run`; stdio is inherited and the exit code is the guest's.
pub fn run_command(rc: &RunCommand) -> Result<u8, String> {
    let engine = engine(Mode::Plain)?;
    let (component, times) = load(&engine, &rc.guest)?;
    emit(
        "guest_compile_ms",
        ms(times.compile.or(times.load_cwasm).unwrap_or_default()),
    );

    let mut b = WasiCtxBuilder::new();
    b.inherit_stdout().inherit_stderr();
    for (k, v) in &rc.env {
        b.env(k, v);
    }
    for (host, guest) in &rc.dirs {
        b.preopened_dir(host, guest, FsPerms::ReadOnly).map_err(s)?;
    }
    // Without --allow-tcp the list is empty and every connect is refused; name lookup stays off.
    let allowed = rc.allow_tcp.clone();
    b.socket_addr_check(move |addr, _use| {
        let ok = allowed.contains(&addr);
        Box::pin(async move { ok })
    });
    let mut store = Store::new(
        &engine,
        State {
            ctx: b.build(),
            table: ResourceTable::new(),
        },
    );
    let linker = linker(&engine)?;
    let command = Command::instantiate(&mut store, &component, &linker).map_err(s)?;

    let start = Instant::now();
    let result = command.wasi_cli_run().call_run(&mut store);
    emit("guest_run_ms", ms(start.elapsed()));
    emit(
        "guest_rss_kib",
        rss_kib().map_or("n/a".to_owned(), |v| v.to_string()),
    );
    match result {
        Ok(Ok(())) => Ok(0),
        Ok(Err(())) => Ok(1),
        Err(e) => match e.downcast_ref::<I32Exit>() {
            Some(I32Exit(code)) => Ok(u8::try_from(*code).unwrap_or(1)),
            None => Err(format!("trap: {e}")),
        },
    }
}
