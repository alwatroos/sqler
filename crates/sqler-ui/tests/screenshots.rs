// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The screenshot harness: renders scripted scenes of the real `WorkspaceView` in both themes
//! to PNG files with GPUI's headless renderer (Metal on macOS, wgpu on Linux).
//!
//! ```text
//! cargo test -p sqler-ui --features screenshots --test screenshots
//! ```
//!
//! Environment:
//! - `SQLER_SCREENSHOT_DIR`: output directory, default `<workspace>/target/screenshots`; a
//!   relative value is joined to the workspace root.
//! - `SQLER_SCREENSHOT_SCENES`: comma-separated scene names; an unknown name fails.
//! - `SQLER_SCREENSHOT_OPTIONAL=1`: skip with exit 0 when no headless renderer exists (a
//!   developer without Vulkan; CI never sets it).
//!
//! Output is `<scene>-<theme>.png`, 1280x800. The pictures are for judging the look, not goldens
//! (ADR-0047): timings in the results differ between runs.
//!
//! The test has `harness = false`: the macOS text system must be built on the main thread.

// A run prints its progress and its failures as lines.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::cell::RefCell;
use std::path::PathBuf;
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{px, size};
use image::imageops::FilterType;
use image::{ImageFormat, RgbaImage};
use sqler_driver_api::sqler_core::{ConnectionConfig, DriverId, Query};
use sqler_driver_api::{BoxFuture, DriverRegistry, Session, collect};
use sqler_driver_sqlite::SqliteDriver;
use sqler_ui::browser::{ConnectFailure, ConnectRequest, Connector, HostKeyPrompt};
use sqler_ui::workspace::scene::{self, SCENE_WAIT, SCENES, Scene, ScenePlayer, ScenePoll};

/// How long the whole run may take before the watchdog ends it.
const WATCHDOG: Duration = Duration::from_secs(600);
/// How many frames are drawn after a scene is ready, before it is captured.
const SETTLE_FRAMES: usize = 8;
/// The fewest distinct colours a picture needs to count as drawn.
const MIN_COLOURS: usize = 16;

/// The tables every session of the harness starts with: made-up companies, no personal data.
const FIXTURE_SQL: &[&str] = &[
    "create table customers (id integer primary key, name text not null, country text)",
    "insert into customers (id, name, country) values \
        (1, 'Northwind Traders', 'US'), (2, 'Contoso Ltd', 'GB'), (3, 'Fabrikam Inc', 'US'), \
        (4, 'Adventure Works', 'CA'), (5, 'Tailspin Toys', 'DE'), (6, 'Wingtip Supplies', 'NL'), \
        (7, 'Litware Systems', 'SE'), (8, 'Proseware Corp', 'FR')",
    "create table orders (id integer primary key, customer_id integer not null \
        references customers (id), total real not null, placed_on text not null)",
    "insert into orders (id, customer_id, total, placed_on) values \
        (1, 1, 120.50, '2026-01-03'), (2, 2, 89.99, '2026-01-04'), (3, 3, 1500.00, '2026-01-09'), \
        (4, 1, 42.00, '2026-01-12'), (5, 4, 310.25, '2026-01-15'), (6, 5, 75.10, '2026-01-21'), \
        (7, 6, 980.00, '2026-02-02'), (8, 7, 23.40, '2026-02-07'), (9, 8, 640.75, '2026-02-11'), \
        (10, 2, 2210.00, '2026-02-18'), (11, 3, 18.00, '2026-02-25'), (12, 5, 455.55, '2026-03-01')",
    "create view big_orders as select * from orders where total >= 500",
];

/// A `Connector` over the `SQLite` driver that seeds every new in-memory database.
struct SeededSqlite(Arc<DriverRegistry>);

impl Connector for SeededSqlite {
    fn driver_ids(&self) -> Vec<DriverId> {
        self.0.ids().cloned().collect()
    }

    fn connect(
        &self,
        request: ConnectRequest,
    ) -> BoxFuture<'static, Result<Box<dyn Session>, ConnectFailure>> {
        let registry = Arc::clone(&self.0);
        let config = ConnectionConfig::new(
            request.spec.name.clone(),
            request.spec.driver.clone(),
            request.spec.target.clone(),
        );
        Box::pin(async move {
            let mut session = registry
                .connect(&config)
                .await
                .map_err(ConnectFailure::Driver)?;
            for sql in FIXTURE_SQL {
                let query = Query::new(*sql);
                let mut stream = session
                    .execute(&query)
                    .await
                    .map_err(ConnectFailure::Driver)?;
                collect(&mut *stream)
                    .await
                    .map_err(ConnectFailure::Driver)?;
            }
            Ok(session)
        })
    }

    fn trust_host_key(
        &self,
        _key: HostKeyPrompt,
    ) -> BoxFuture<'static, Result<(), ConnectFailure>> {
        Box::pin(std::future::ready(Ok(())))
    }
}

/// The workspace root, from this crate's manifest directory.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn output_dir() -> PathBuf {
    match std::env::var_os("SQLER_SCREENSHOT_DIR") {
        Some(dir) => workspace_root().join(dir),
        None => workspace_root().join("target/screenshots"),
    }
}

/// The size gate: the picture is exactly `want`.
fn check_size(image: &RgbaImage, want: (u32, u32)) -> Result<(), String> {
    if image.dimensions() == want {
        Ok(())
    } else {
        let (w, h) = image.dimensions();
        Err(format!("size {w}x{h}, expected {}x{}", want.0, want.1))
    }
}

/// The colour gate: at least [`MIN_COLOURS`] distinct pixel values. Returns how many it saw
/// (it stops counting at the minimum).
fn check_colours(image: &RgbaImage) -> Result<usize, String> {
    let mut seen: Vec<[u8; 4]> = Vec::with_capacity(MIN_COLOURS);
    for pixel in image.pixels() {
        if !seen.contains(&pixel.0) {
            seen.push(pixel.0);
            if seen.len() >= MIN_COLOURS {
                return Ok(seen.len());
            }
        }
    }
    Err(format!(
        "only {} distinct colours, expected at least {MIN_COLOURS}",
        seen.len()
    ))
}

/// A blank picture must fail both gates, or the gates are worth nothing.
fn self_check() -> Result<(), String> {
    let blank = RgbaImage::from_pixel(4, 4, image::Rgba([10, 20, 30, 255]));
    if check_size(&blank, (1280, 800)).is_ok() {
        return Err("self-check: a 4x4 picture passed the size gate".to_owned());
    }
    if check_colours(&blank).is_ok() {
        return Err("self-check: a one-colour picture passed the colour gate".to_owned());
    }
    Ok(())
}

fn main() -> ExitCode {
    if std::env::args().any(|arg| arg == "--list") {
        // nextest lists harness = false binaries with `--list`; this one has no listed tests.
        return ExitCode::SUCCESS;
    }
    if cfg!(windows) {
        println!("skipped: no headless renderer on windows");
        return ExitCode::SUCCESS;
    }
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

/// The scenes to render, from `SQLER_SCREENSHOT_SCENES` or all of them.
fn chosen_scenes() -> Result<Vec<&'static Scene>, String> {
    let list = std::env::var("SQLER_SCREENSHOT_SCENES").unwrap_or_default();
    if list.trim().is_empty() {
        return Ok(SCENES.iter().collect());
    }
    list.split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(|name| scene::find(name).ok_or_else(|| format!("unknown scene: {name}")))
        .collect()
}

/// Renders everything. `Ok(true)` when every picture was written.
fn run() -> Result<bool, String> {
    self_check()?;
    std::thread::spawn(|| {
        std::thread::sleep(WATCHDOG);
        eprintln!("error: the screenshot run took longer than {WATCHDOG:?}");
        std::process::exit(1);
    });
    let scenes = chosen_scenes()?;

    // On macOS a missing Metal device ends the process inside GPUI; this only catches Linux.
    match gpui_platform::current_headless_renderer() {
        Ok(Some(_)) => {}
        Ok(None) => {
            println!("skipped: no headless renderer on this platform");
            return Ok(true);
        }
        Err(err) => {
            if std::env::var_os("SQLER_SCREENSHOT_OPTIONAL").is_some_and(|v| v == "1") {
                println!("skipped: headless renderer unavailable: {err}");
                return Ok(true);
            }
            return Err(format!("headless renderer unavailable: {err}"));
        }
    }

    let dir = output_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|err| format!("cannot create {}: {err}", dir.display()))?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|err| format!("cannot start the tokio runtime: {err}"))?;
    let mut registry = DriverRegistry::new();
    registry
        .register(Box::new(SqliteDriver::new()))
        .map_err(|err| format!("cannot register the sqlite driver: {err}"))?;
    let connector: Arc<dyn Connector> = Arc::new(SeededSqlite(Arc::new(registry)));

    // One platform for the whole run: on Linux each one leaks a dispatcher pool.
    let platform = gpui_platform::current_platform(true);
    let text = platform.text_system();

    let mut ok = true;
    for scene in scenes {
        if let Some(pending) = scene.pending {
            println!("pending: {}: {pending}", scene.name);
        }
        for theme in scene::SceneTheme::ALL {
            let label = format!("{}-{}", scene.name, theme.name());
            let mut context = gpui::HeadlessAppContext::with_platform(
                text.clone(),
                Arc::new(sqler_ui::theme::icons::AppAssets),
                gpui_platform::current_headless_renderer,
            );
            context.allow_parking();
            context.update(sqler_ui::workspace::init);
            match capture(&mut context, scene, theme, &connector, &runtime, &dir) {
                Ok(line) => println!("ok: {label} {line}"),
                Err(reason) => {
                    ok = false;
                    println!("FAILED: {label}: {reason}");
                }
            }
        }
    }
    drop(platform);
    Ok(ok)
}

/// Opens the scene, plays it, captures it and writes the PNG. Returns the log suffix.
fn capture(
    context: &mut gpui::HeadlessAppContext,
    scene: &'static Scene,
    theme: scene::SceneTheme,
    connector: &Arc<dyn Connector>,
    runtime: &tokio::runtime::Runtime,
    dir: &std::path::Path,
) -> Result<String, String> {
    let (width, height) = scene::WINDOW_SIZE;
    let player: Rc<RefCell<Option<ScenePlayer>>> = Rc::new(RefCell::new(None));
    let slot = Rc::clone(&player);
    let connector = Arc::clone(connector);
    let handle_runtime = runtime.handle().clone();
    let handle = context
        .open_window(size(px(width), px(height)), move |window, cx| {
            let (root, scene_player) =
                scene::open(scene, theme, connector, handle_runtime, window, cx);
            *slot.borrow_mut() = Some(scene_player);
            root
        })
        .map_err(|err| format!("cannot open the window: {err:#}"))?;
    let any = gpui::AnyWindowHandle::from(handle);

    let deadline = Instant::now() + SCENE_WAIT * 2 + Duration::from_secs(5);
    let note = loop {
        context.run_until_parked();
        let poll = context
            .update_window(any, |_, window, cx| {
                // Editors and lists are created while the window renders.
                window.draw(cx).clear(cx);
                match player.borrow_mut().as_mut() {
                    Some(player) => player.poll(window, cx),
                    None => ScenePoll::Failed("the scene did not open".to_owned()),
                }
            })
            .map_err(|err| format!("the window is gone: {err:#}"))?;
        match poll {
            ScenePoll::Ready { note } => break note,
            ScenePoll::Failed(reason) => return Err(reason),
            ScenePoll::Waiting => {
                if Instant::now() > deadline {
                    return Err("the scene did not finish in time".to_owned());
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    };

    // The list measures its rows on the first frame, and streamed rows reach the grid on the next
    // turns of the executors, so draw a few times with short pauses.
    for _ in 0..SETTLE_FRAMES {
        std::thread::sleep(Duration::from_millis(30));
        // Timers (the grid's row sync, debounces) only fire when the test clock moves.
        context.advance_clock(Duration::from_millis(100));
        context.run_until_parked();
        context
            .update_window(any, |_, window, cx| {
                window.draw(cx).clear(cx);
            })
            .map_err(|err| format!("the window is gone: {err:#}"))?;
    }
    let scale = context
        .update_window(any, |_, window, _| window.scale_factor())
        .map_err(|err| format!("the window is gone: {err:#}"))?;
    let raw = context
        .capture_screenshot(any)
        .map_err(|err| format!("capture failed: {err:#}"))?;

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // 1280 * 2.0
    let want = (
        (width * scale).round() as u32,
        (height * scale).round() as u32,
    );
    check_size(&raw, want)?;
    let colours = check_colours(&raw)?;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // 1280 x 800
    let (out_w, out_h) = (width as u32, height as u32);
    let small = image::imageops::resize(&raw, out_w, out_h, FilterType::Triangle);
    check_size(&small, (out_w, out_h))?;
    let path = dir.join(format!("{}-{}.png", scene.name, theme.name()));
    image::save_buffer_with_format(
        &path,
        small.as_raw(),
        out_w,
        out_h,
        image::ColorType::Rgba8,
        ImageFormat::Png,
    )
    .map_err(|err| format!("cannot write {}: {err}", path.display()))?;
    let note = note.map(|n| format!(" [{n}]")).unwrap_or_default();
    Ok(format!("{out_w}x{out_h} ({colours}+ colours){note}"))
}
