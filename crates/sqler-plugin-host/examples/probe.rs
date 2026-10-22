// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Measures the plugin runtime on one plugin directory: `cargo run --release --example probe --features cl <dir>`.
//!
//! Prints the engine creation and compile time (one each, cold), then the instantiate and `init` timings over a
//! number of fresh instances. The numbers feed ADR-0057; they are not asserted anywhere.

#![allow(clippy::print_stdout, clippy::print_stderr)]

#[cfg(feature = "cl")]
fn main() {
    if let Err(e) = run() {
        eprintln!("probe: {e}");
        std::process::exit(1);
    }
}

#[cfg(not(feature = "cl"))]
fn main() {
    eprintln!("probe: build with `--features cl`");
}

#[cfg(feature = "cl")]
fn run() -> Result<(), String> {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use sqler_plugin_api::{CapabilitySet, GrantKey, Limits};
    use sqler_plugin_host::{
        GrantSource, HostOptions, MemorySettings, NoEgress, PluginDirs, PluginHost, Services,
        discover,
    };

    /// Approves what the manifest declares.
    struct DeclaredGrants(Vec<(GrantKey, CapabilitySet)>);

    impl GrantSource for DeclaredGrants {
        fn approved(&self, key: &GrantKey) -> Option<CapabilitySet> {
            self.0
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, s)| s.clone())
        }
    }

    const ROUNDS: usize = 200;

    fn percentile(sorted: &[Duration], p: usize) -> Duration {
        sorted
            .get(sorted.len().saturating_sub(1) * p / 100)
            .copied()
            .unwrap_or_default()
    }

    let dir: PathBuf = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or("usage: probe <plugin-dir>")?;
    let d = discover(&PluginDirs {
        dev: vec![dir],
        ..PluginDirs::default()
    });
    if let Some(p) = d.problems.first() {
        return Err(format!("{}: {}", p.dir.display(), p.error));
    }
    let plugin = d.plugins.first().ok_or("no plugin found")?;
    let id = plugin.manifest.id.clone();
    let grants = DeclaredGrants(
        d.plugins
            .iter()
            .map(|p| (p.key.clone(), p.manifest.capabilities.clone()))
            .collect(),
    );
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let host = PluginHost::new(HostOptions {
        runtime: rt.handle().clone(),
        grants: Arc::new(grants),
        settings: Arc::new(MemorySettings::default()),
        egress: Arc::new(NoEgress),
        limits: Limits::DEFAULT,
        services: Services::new(),
    });
    let problems = host.load(d);
    if let Some(p) = problems.first() {
        return Err(format!("{}: {}", p.dir.display(), p.error));
    }

    let cold = Instant::now();
    rt.block_on(host.init(&id)).map_err(|e| e.to_string())?;
    let cold = cold.elapsed();
    for _ in 0..ROUNDS {
        host.refresh_grants(&id);
        rt.block_on(host.init(&id)).map_err(|e| e.to_string())?;
    }
    host.shutdown();

    let m = host.metrics();
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    println!("plugin           {id}");
    println!(
        "engine-create    {:.2} ms",
        ms(m.engine_create.unwrap_or_default())
    );
    println!(
        "compile          {:.2} ms",
        ms(m.compile.first().copied().unwrap_or_default())
    );
    println!(
        "first init call  {:.2} ms (engine + compile + instantiate + init)",
        ms(cold)
    );
    let mut instantiate = m.instantiate.clone();
    instantiate.sort();
    let mut first_init = m.init.clone();
    first_init.sort();
    println!(
        "instantiate      p50 {} us, p95 {} us ({} samples)",
        percentile(&instantiate, 50).as_micros(),
        percentile(&instantiate, 95).as_micros(),
        instantiate.len()
    );
    println!(
        "init             p50 {} ns, p95 {} ns ({} samples)",
        percentile(&first_init, 50).as_nanos(),
        percentile(&first_init, 95).as_nanos(),
        first_init.len()
    );
    Ok(())
}
