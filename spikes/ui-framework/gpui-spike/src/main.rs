// SPDX-License-Identifier: Apache-2.0
//! Throwaway GPUI + gpui-component spike for ADR-0001: a 1M-row grid and a SQL editor.
//!
//! `spike-gpui` opens the window, `--exit-after-first-frame` measures cold start,
//! `--smoke` runs the scripted checks and prints the `SPIKE key=value` report.

mod data;
mod editor;
mod grid;
mod smoke;

use std::rc::Rc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use gpui::{
    App, AppContext as _, Bounds, Context, Entity, IntoElement, ParentElement as _, Render,
    Styled as _, Window, WindowBounds, WindowOptions, div, px, size,
};
use gpui_component::Root;
use gpui_component::input::EditorState;
use gpui_component::resizable::{h_resizable, resizable_panel};
use gpui_component::table::{DataTable, TableState};

use grid::{FrameStats, GridDelegate};
use smoke::Report;

static START: OnceLock<Instant> = OnceLock::new();

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Interactive,
    ExitAfterFirstFrame,
    Smoke,
}

/// Phase trace on stderr when `SPIKE_TRACE` is set; the report on stdout is unaffected.
fn trace(what: &str) {
    if std::env::var_os("SPIKE_TRACE").is_some() {
        eprintln!("[{:8.1} ms] {what}", ms_since_start());
    }
}

fn ms_since_start() -> f64 {
    START
        .get()
        .map_or(0.0, |s| s.elapsed().as_secs_f64() * 1000.0)
}

struct SpikeView {
    table: Entity<TableState<GridDelegate>>,
    editor: Entity<EditorState>,
}

impl SpikeView {
    fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        stats: &Rc<FrameStats>,
        mode: Mode,
    ) -> Self {
        let delegate = GridDelegate::new(stats.clone());
        let table = cx.new(|cx| {
            TableState::new(delegate, window, cx)
                .sortable(false)
                .col_movable(false)
                .col_resizable(false)
        });
        let editor = editor::new_editor(window, cx);
        let this = Self { table, editor };
        match mode {
            Mode::Interactive => {}
            Mode::ExitAfterFirstFrame => frame_hook(window, stats.clone(), mode),
            Mode::Smoke => {
                frame_hook(window, stats.clone(), mode);
                this.spawn_smoke(window, cx, stats.clone());
            }
        }
        this
    }

    fn spawn_smoke(&self, window: &mut Window, cx: &mut Context<Self>, stats: Rc<FrameStats>) {
        let table = self.table.clone();
        let editor = self.editor.clone();
        cx.spawn_in(window, async move |_, cx| {
            let wait_frames = async |n: u64, cx: &mut gpui::AsyncWindowContext| {
                let target = stats.frames() + n;
                while stats.frames() < target {
                    cx.background_executor()
                        .timer(Duration::from_millis(10))
                        .await;
                }
            };
            // First frame, then the two scripted scrolls.
            wait_frames(1, cx).await;
            trace("first frame seen by driver");
            for row in [500_000, data::ROW_COUNT - 1] {
                table.update(cx, |t, cx| t.scroll_to_row(row, cx));
                wait_frames(2, cx).await;
                trace(&format!("scrolled to row {row}"));
            }
            // Give the virtual list a bounded extra time to land on the last row.
            let deadline = Instant::now() + Duration::from_secs(5);
            while !stats.saw_last() && Instant::now() < deadline {
                wait_frames(1, cx).await;
            }
            let language_ok = editor.read_with(cx, |e, _| e.language_name() == editor::LANGUAGE);
            let kinds = if language_ok {
                u32::try_from(editor::highlight_kinds(editor::SAMPLE_SQL).len()).unwrap_or(0)
            } else {
                0
            };
            let (display, renderer) = cx
                .update(|window, cx| (display_name(cx), renderer_name(window)))
                .unwrap_or_else(|_| ("unknown".to_owned(), "unknown".to_owned()));
            cx.background_executor().timer(Duration::from_secs(1)).await;
            let report = Report {
                display,
                renderer,
                first_frame_ms: stats.first_frame_ms().unwrap_or(0.0),
                grid_rows: data::ROW_COUNT,
                grid_max_rows_rendered_per_frame: stats.max_rows(),
                grid_rendered_last_row: stats.saw_last(),
                editor_highlight_kinds: kinds,
                rss_mb: smoke::rss_mb(),
            };
            smoke::finish(&report);
        })
        .detach();
    }
}

/// Runs after every presented frame; keeps frames coming in smoke mode.
fn frame_hook(window: &Window, stats: Rc<FrameStats>, mode: Mode) {
    window.on_next_frame(move |window, _cx| {
        stats.end_frame(ms_since_start());
        match mode {
            Mode::ExitAfterFirstFrame => {
                println!("SPIKE first_frame_ms={:.1}", ms_since_start());
                std::process::exit(0);
            }
            _ => frame_hook(window, stats, mode),
        }
    });
}

fn display_name(cx: &App) -> String {
    if cfg!(target_os = "macos") {
        "macos".to_owned()
    } else if cfg!(target_os = "windows") {
        "windows".to_owned()
    } else if cx.compositor_name().to_lowercase().contains("wayland") {
        "wayland".to_owned()
    } else {
        "x11".to_owned()
    }
}

fn renderer_name(window: &Window) -> String {
    match window.gpu_specs() {
        Some(s) => format!(
            "{} / {}{}",
            s.device_name,
            s.driver_name,
            if s.is_software_emulated {
                " (software)"
            } else {
                ""
            }
        ),
        None if cfg!(target_os = "macos") => "metal".to_owned(),
        None => "unknown".to_owned(),
    }
}

impl Render for SpikeView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(
            h_resizable("split")
                .child(
                    resizable_panel()
                        .size(px(760.))
                        .child(DataTable::new(&self.table).stripe(true).bordered(true)),
                )
                .child(
                    resizable_panel().child(
                        gpui_component::input::Editor::new(&self.editor)
                            .h_full()
                            .bordered(false),
                    ),
                ),
        )
    }
}

fn parse_mode() -> Result<Mode, String> {
    let mut mode = Mode::Interactive;
    for arg in std::env::args().skip(1) {
        mode = match arg.as_str() {
            "--smoke" => Mode::Smoke,
            "--exit-after-first-frame" => Mode::ExitAfterFirstFrame,
            other => return Err(format!("unknown argument: {other}")),
        };
    }
    Ok(mode)
}

fn main() {
    let _ = START.set(Instant::now());
    let mode = match parse_mode() {
        Ok(mode) => mode,
        Err(msg) => {
            eprintln!("{msg}\nusage: spike-gpui [--smoke | --exit-after-first-frame]");
            std::process::exit(1);
        }
    };
    if mode != Mode::Interactive {
        smoke::spawn_watchdog();
    }
    let stats = FrameStats::new();
    gpui_platform::application().run(move |cx: &mut App| {
        gpui_component::init(cx);
        cx.activate(true);
        let bounds = Bounds::centered(None, size(px(1280.), px(800.)), cx);
        let opened = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| {
                let view = cx.new(|cx| SpikeView::new(window, cx, &stats, mode));
                cx.new(|cx| Root::new(view, window, cx))
            },
        );
        if let Err(err) = opened {
            smoke::fail("window-open");
            eprintln!("open_window failed: {err:#}");
            std::process::exit(1);
        }
    });
}
