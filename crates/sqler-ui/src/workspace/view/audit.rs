// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `GPUI` side of the AI audit viewer (ADR-0037, ADR-0050): the "Audit" view of the bottom
//! panel and the effect that reads a page of the log.
//!
//! The panel only renders what `AuditPanel` holds, which is already scrubbed. It is read-only:
//! there is no edit or delete affordance. The read runs on the blocking pool, never on the UI
//! thread and never at startup.

use std::ops::Range;
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use gpui::{
    AnyElement, ClickEvent, Context, InteractiveElement as _, IntoElement as _, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, div, px, uniform_list,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::menu::DropdownMenu as _;
use gpui_component::{ActiveTheme as _, Disableable as _, Sizable as _};

use super::WorkspaceView;
use crate::ai::{
    AUDIT_DECISIONS, AUDIT_SURFACES, AUDIT_TOOLS, AiAction, AuditBatch, AuditFilterChange,
    AuditLoad, AuditRead, AuditRow, AuditTone, audit_row, decision_label, surface_label,
};
use crate::theme::SPACE;
use crate::theme::icons::AppIcon;
use crate::theme::widgets::empty_state;
use crate::workspace::chrome::{RowState, TOOLBAR_HEIGHT, hinted_item, list_row, ui_scale};

/// Width of the "when" column.
const WHEN_W: f32 = 80.;
/// Width of the "surface" column.
const SURFACE_W: f32 = 110.;
/// Width of the "connection" column.
const CONNECTION_W: f32 = 90.;
/// Width of the "tool" column.
const TOOL_W: f32 = 150.;
/// Width of the "decision" column.
const DECISION_W: f32 = 80.;
/// Width of the "result" column.
const RESULT_W: f32 = 130.;

impl WorkspaceView {
    /// Reads a page of the audit log on the blocking pool and reports it to the workspace. A
    /// stale answer is dropped there by its request number.
    pub(super) fn read_audit(&mut self, read: AuditRead, cx: &mut Context<Self>) {
        let AuditRead { request, query } = read;
        let Some(log) = self.audit_log.clone() else {
            // No config directory: there is no log, so the view is empty.
            self.ws.audit_loaded(request, Ok(AuditBatch::default()));
            cx.notify();
            return;
        };
        let done = self.bridge.blocking(move || {
            log.read(&query)
                .map(AuditBatch::from)
                .map_err(|e| e.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = done
                .await
                .unwrap_or_else(|_| Err("the audit read was lost".to_owned()));
            this.update(cx, |view, cx| {
                view.ws.audit_loaded(request, result);
                view.settle(cx);
            })
            .ok();
        })
        .detach();
    }
}

/// The text of the connection filter button.
fn connection_label(view: &WorkspaceView) -> String {
    let panel = view.workspace().audit();
    match panel.filters().connection.as_deref() {
        None => "All connections".to_owned(),
        Some(id) => view
            .workspace()
            .audit_connections()
            .into_iter()
            .find(|(c, _)| c == id)
            .map_or_else(|| "Deleted connection".to_owned(), |(_, name)| name),
    }
}

/// A filter button whose menu lists `items` (label and change). The first item clears it.
fn filter_button(
    id: &'static str,
    label: String,
    items: Vec<(String, AuditFilterChange)>,
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    let weak = cx.weak_entity();
    Button::new(id)
        .label(label)
        .icon(AppIcon::ChevronDown)
        .outline()
        .small()
        .dropdown_menu(move |mut menu, _, _| {
            for (text, change) in items.clone() {
                let weak = weak.clone();
                menu = menu.item(hinted_item(text, None, None).on_click(move |_, _, cx| {
                    let change = change.clone();
                    weak.update(cx, |view, cx| {
                        view.dispatch_ai(AiAction::SetAuditFilter(change), cx);
                    })
                    .ok();
                }));
            }
            menu
        })
        .into_any_element()
}

/// The items of one filter menu: label and the change it makes.
type MenuItems = Vec<(String, AuditFilterChange)>;

/// The items of the four filter menus.
fn menu_items(view: &WorkspaceView) -> [MenuItems; 4] {
    let mut connections = vec![(
        "All connections".to_owned(),
        AuditFilterChange::Connection(None),
    )];
    connections.extend(
        view.workspace()
            .audit_connections()
            .into_iter()
            .map(|(id, name)| (name, AuditFilterChange::Connection(Some(id)))),
    );
    let mut surfaces = vec![("All surfaces".to_owned(), AuditFilterChange::Surface(None))];
    surfaces.extend(AUDIT_SURFACES.into_iter().map(|s| {
        (
            surface_label(s).to_owned(),
            AuditFilterChange::Surface(Some(s)),
        )
    }));
    let mut tools = vec![("All tools".to_owned(), AuditFilterChange::Tool(None))];
    tools.extend(AUDIT_TOOLS.iter().map(|t| {
        (
            (*t).to_owned(),
            AuditFilterChange::Tool(Some((*t).to_owned())),
        )
    }));
    let mut decisions = vec![(
        "All decisions".to_owned(),
        AuditFilterChange::Decision(None),
    )];
    decisions.extend(AUDIT_DECISIONS.into_iter().map(|d| {
        (
            decision_label(d).to_owned(),
            AuditFilterChange::Decision(Some(d)),
        )
    }));
    [connections, surfaces, tools, decisions]
}

/// The Audit view of the bottom panel: filters, the list of tool calls and "Load more".
#[allow(clippy::too_many_lines)] // one declarative element tree
pub(crate) fn audit_panel_body(
    view: &WorkspaceView,
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    let ws = view.workspace();
    let panel = ws.audit();
    let theme = cx.theme();
    let (border, muted, danger) = (theme.border, theme.muted_foreground, theme.danger);
    let scale = ui_scale(cx);
    let load = panel.load().clone();
    let filtered = *panel.filters() != crate::ai::AuditFilters::default();
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(0));
    let rows: Rc<Vec<AuditRow>> = Rc::new(
        panel
            .entries()
            .iter()
            .map(|e| audit_row(e, now_ms))
            .collect(),
    );
    let count = rows.len();
    let (skipped, newer) = panel.notes();
    let has_more = panel.has_more();

    let [connections, surfaces, tools, decisions] = menu_items(view);
    let filters = panel.filters();
    let toolbar = div()
        .h(px(TOOLBAR_HEIGHT))
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(SPACE[2]))
        .px(px(SPACE[3]))
        .border_b_1()
        .border_color(border)
        .child(filter_button(
            "audit-connection",
            connection_label(view),
            connections,
            cx,
        ))
        .child(filter_button(
            "audit-surface",
            filters
                .surface
                .map_or("All surfaces", surface_label)
                .to_owned(),
            surfaces,
            cx,
        ))
        .child(filter_button(
            "audit-tool",
            filters
                .tool
                .clone()
                .unwrap_or_else(|| "All tools".to_owned()),
            tools,
            cx,
        ))
        .child(filter_button(
            "audit-decision",
            filters
                .decision
                .map_or("All decisions", decision_label)
                .to_owned(),
            decisions,
            cx,
        ))
        .child(div().flex_1())
        .child(
            Button::new("audit-copy")
                .label("Copy SQL")
                .ghost()
                .small()
                .disabled(panel.selected_sql().is_none())
                .on_click(cx.listener(|view, _: &ClickEvent, _, cx| {
                    view.dispatch_ai(AiAction::CopyAuditSql, cx);
                })),
        );

    let mut note = Vec::new();
    if skipped > 0 {
        note.push(format!("{skipped} unreadable lines skipped"));
    }
    if newer > 0 {
        note.push(format!("{newer} lines from a newer version skipped"));
    }
    let (status, status_color) = match &load {
        AuditLoad::Loading => ("Loading…".to_owned(), muted),
        AuditLoad::Failed(text) => (format!("Could not read the audit log: {text}"), danger),
        _ if count == 0 => (note.join(" · "), muted),
        _ => {
            let mut line = format!("{count} entries");
            if !note.is_empty() {
                line.push_str(" · ");
                line.push_str(&note.join(" · "));
            }
            (line, muted)
        }
    };

    let body = if matches!(load, AuditLoad::NotLoaded) {
        centered(
            empty_state(
                AppIcon::Clock,
                "The audit log is not loaded",
                "Every AI tool call is recorded in a local file.",
                cx,
            )
            .child(
                div().pt(px(SPACE[3])).child(
                    Button::new("audit-load")
                        .label("Load audit log")
                        .outline()
                        .small()
                        .on_click(cx.listener(|view, _: &ClickEvent, _, cx| {
                            view.dispatch_ai(AiAction::OpenAuditLog, cx);
                        })),
                ),
            ),
        )
    } else if count == 0 && !matches!(load, AuditLoad::Loading | AuditLoad::Failed(_)) {
        let (title, line) = if filtered {
            (
                "Nothing matches these filters",
                "Change or clear a filter to see more calls.",
            )
        } else {
            (
                "No AI tool calls yet",
                "Calls made by the assistant, MCP clients and agents are listed here.",
            )
        };
        centered(empty_state(AppIcon::Clock, title, line, cx))
    } else {
        uniform_list(
            "audit-rows",
            count,
            cx.processor(move |view, range: Range<usize>, _window, cx| {
                let selected = view.workspace().audit().selected();
                range
                    .filter_map(|ix| rows.get(ix).map(|row| (ix, row)))
                    .map(|(ix, row)| audit_line(ix, row, selected == Some(ix), cx))
                    .collect::<Vec<_>>()
            }),
        )
        .flex_1()
        .min_h_0()
        .w_full()
        .into_any_element()
    };

    let more = has_more.then(|| {
        div()
            .flex_none()
            .flex()
            .justify_center()
            .py(px(SPACE[1]))
            .border_t_1()
            .border_color(border)
            .child(
                Button::new("audit-more")
                    .label("Load more")
                    .ghost()
                    .small()
                    .disabled(matches!(load, AuditLoad::Loading))
                    .on_click(cx.listener(|view, _: &ClickEvent, _, cx| {
                        view.dispatch_ai(AiAction::LoadMoreAudit, cx);
                    })),
            )
    });

    div()
        .id("audit-panel")
        .flex_1()
        .min_h_0()
        .w_full()
        .flex()
        .flex_col()
        .child(toolbar)
        .children((!status.is_empty()).then(|| {
            div()
                .px(px(SPACE[3]))
                .py(px(SPACE[1]))
                .text_size(px(scale.xs))
                .text_color(status_color)
                .child(status)
        }))
        .child(body)
        .children(more)
        .into_any_element()
}

/// Centres an empty state in the space of the list.
fn centered(content: gpui::Div) -> AnyElement {
    div()
        .flex_1()
        .min_h_0()
        .w_full()
        .flex()
        .items_center()
        .justify_center()
        .child(content)
        .into_any_element()
}

/// One line of the audit list.
fn audit_line(
    ix: usize,
    row: &AuditRow,
    selected: bool,
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    let theme = cx.theme();
    let (muted, danger, success) = (theme.muted_foreground, theme.danger, theme.success);
    let mono = theme.mono_font_family.clone();
    let scale = ui_scale(cx);
    let tone = match row.tone {
        AuditTone::Muted => muted,
        AuditTone::Success => success,
        AuditTone::Danger => danger,
    };
    let result_color = if row.result.starts_with("Error") {
        danger
    } else {
        muted
    };
    let state = RowState {
        selected,
        // The list has no focus handle of its own: the selection draws unfocused.
        focused: false,
        disabled: false,
    };
    let cell = |w: f32, text: String, color: Option<gpui::Hsla>| {
        let el = div()
            .w(px(w))
            .flex_none()
            .text_size(px(scale.xs))
            .truncate()
            .child(text);
        match color {
            Some(c) => el.text_color(c),
            None => el,
        }
    };
    let mut line = list_row(("audit-row", ix), state, cx)
        .w_full()
        .gap(px(SPACE[3]))
        .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
            view.dispatch_ai(AiAction::SelectAuditEntry(ix), cx);
        }))
        .child(cell(WHEN_W, row.when.clone(), Some(muted)))
        .child(cell(SURFACE_W, row.surface.to_owned(), Some(muted)))
        .child(cell(CONNECTION_W, row.connection.clone(), None))
        .child(cell(TOOL_W, row.tool.clone(), None))
        .child(cell(DECISION_W, row.decision.to_owned(), Some(tone)))
        .child(cell(RESULT_W, row.result.clone(), Some(result_color)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .font_family(mono)
                .text_size(px(scale.mono))
                .truncate()
                .child(row.sql.clone()),
        );
    if row.more_statements > 0 {
        line = line.child(
            div()
                .flex_none()
                .text_size(px(scale.xs))
                .text_color(muted)
                .child(format!("+{}", row.more_statements)),
        );
    }
    line.into_any_element()
}
