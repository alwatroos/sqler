// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `GPUI` side of the import wizard (ADR-0067): the panel of each step and the carrying out
//! of `ImportEffect`s (finding the default files, the open-file prompt, reading and parsing).
//!
//! The panel only renders `ImportWizard` state; every click becomes a
//! `ConnectionAction::Import`. The password fields are masked `InputState`s owned by
//! `DialogInputs`. Nothing here logs wizard state: it can hold paths and typed secrets.

use gpui::{
    AnyElement, ClickEvent, Context, Div, ElementId, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, PathPromptOptions, SharedString, StatefulInteractiveElement as _,
    Styled as _, div, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::Input;
use gpui_component::spinner::Spinner;
use gpui_component::{ActiveTheme as _, Disableable as _, Icon, Sizable as _};
use sqler_import::HostEnv;

use super::WorkspaceView;
use crate::connections::import::load::{discover_all, load};
use crate::connections::view::DialogInputs;
use crate::connections::{
    ConnectionAction, EntryBadge, ImportAction, ImportEffect, ImportStep, ImportWizard,
    NOT_FOUND_TEXT, SCANNING_TEXT, SECRETS_HINT,
};
use crate::theme::SPACE;
use crate::theme::icons::AppIcon;
use crate::workspace::chrome::{
    ICON_SM, LABEL_WIDTH, LIST_MAX_H, RowState, dialog_footer, dialog_title, field_row, list_row,
    ui_scale,
};

/// Theme colours the panel uses, copied out of the theme.
#[derive(Clone, Copy)]
struct Tones {
    muted: Hsla,
    danger: Hsla,
    success: Hsla,
}

impl Tones {
    fn of(cx: &Context<WorkspaceView>) -> Self {
        let theme = cx.theme();
        Self {
            muted: theme.muted_foreground,
            danger: theme.danger,
            success: theme.success,
        }
    }
}

/// A dialog button that dispatches an import action (or `Cancel`).
fn action_button(
    id: &'static str,
    label: impl Into<SharedString>,
    action: ConnectionAction,
    primary: bool,
    enabled: bool,
    cx: &mut Context<WorkspaceView>,
) -> AnyElement {
    let button = Button::new(id).label(label).small();
    let button = if primary {
        button.primary()
    } else {
        button.outline()
    };
    button
        .disabled(!enabled)
        .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
            view.dispatch_connection(action.clone(), cx);
        }))
        .into_any_element()
}

fn import(action: ImportAction) -> ConnectionAction {
    ConnectionAction::Import(action)
}

fn icon(kind: AppIcon, color: Hsla) -> Icon {
    Icon::new(kind).size(px(ICON_SM)).text_color(color)
}

/// A line with a spinner in front.
fn busy_line(text: &'static str, tones: Tones, cx: &Context<WorkspaceView>) -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(SPACE[3]))
        .text_size(px(ui_scale(cx).sm))
        .text_color(tones.muted)
        .child(
            Spinner::new()
                .icon(AppIcon::LoaderCircle)
                .color(tones.muted)
                .xsmall(),
        )
        .child(text)
}

/// A line of small muted text.
fn note(text: impl Into<SharedString>, tones: Tones, cx: &Context<WorkspaceView>) -> Div {
    div()
        .text_size(px(ui_scale(cx).xs))
        .text_color(tones.muted)
        .child(text.into())
}

fn list_column(id: &'static str) -> gpui::Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_col()
        .gap(px(SPACE[0]))
        .max_h(px(LIST_MAX_H))
        .overflow_y_scroll()
}

impl WorkspaceView {
    /// The wizard's panel for its current step. `inputs` holds the password fields on the
    /// Secrets step.
    pub(crate) fn import_panel(
        wizard: &ImportWizard,
        inputs: Option<&DialogInputs>,
        cx: &mut Context<WorkspaceView>,
    ) -> Div {
        let tones = Tones::of(cx);
        let (heading, body, buttons) = match wizard.step() {
            ImportStep::Source => (
                "Import connections",
                Self::source_body(wizard, tones, cx),
                Self::source_buttons(wizard, cx),
            ),
            ImportStep::Preview => (
                "Choose what to import",
                Self::preview_body(wizard, tones, cx),
                Self::preview_buttons(wizard, cx),
            ),
            ImportStep::Secrets => (
                "Passwords",
                Self::secrets_body(wizard, inputs, tones, cx),
                vec![
                    action_button(
                        "import-cancel",
                        "Cancel",
                        ConnectionAction::Cancel,
                        false,
                        true,
                        cx,
                    ),
                    action_button(
                        "import-back",
                        "Back",
                        import(ImportAction::Back),
                        false,
                        wizard.can_back(),
                        cx,
                    ),
                    action_button(
                        "import-apply",
                        "Import",
                        import(ImportAction::Apply),
                        true,
                        true,
                        cx,
                    ),
                ],
            ),
            ImportStep::Summary => (
                "Import finished",
                Self::summary_body(wizard, tones, cx),
                vec![action_button(
                    "import-close",
                    "Close",
                    import(ImportAction::Next),
                    true,
                    true,
                    cx,
                )],
            ),
        };
        div()
            .flex()
            .flex_col()
            .min_h_0()
            .gap(px(SPACE[4]))
            .child(dialog_title(Some(AppIcon::FolderOpen), heading, cx))
            .child(body)
            .child(dialog_footer(cx).children(buttons))
    }

    fn source_buttons(wizard: &ImportWizard, cx: &mut Context<WorkspaceView>) -> Vec<AnyElement> {
        let idle = !wizard.scanning() && !wizard.loading();
        vec![
            action_button(
                "import-cancel",
                "Cancel",
                ConnectionAction::Cancel,
                false,
                true,
                cx,
            ),
            action_button(
                "import-choose",
                "Choose file...",
                import(ImportAction::ChooseFile),
                false,
                idle && !wizard.options().is_empty(),
                cx,
            ),
            action_button(
                "import-next",
                "Next",
                import(ImportAction::Next),
                true,
                wizard.can_next(),
                cx,
            ),
        ]
    }

    fn preview_buttons(wizard: &ImportWizard, cx: &mut Context<WorkspaceView>) -> Vec<AnyElement> {
        vec![
            action_button(
                "import-cancel",
                "Cancel",
                ConnectionAction::Cancel,
                false,
                true,
                cx,
            ),
            action_button(
                "import-select-none",
                "Select none",
                import(ImportAction::SelectNone),
                false,
                true,
                cx,
            ),
            action_button(
                "import-select-all",
                "Select all",
                import(ImportAction::SelectAll),
                false,
                true,
                cx,
            ),
            action_button(
                "import-back",
                "Back",
                import(ImportAction::Back),
                false,
                wizard.can_back(),
                cx,
            ),
            action_button(
                "import-next",
                format!("Next ({})", wizard.selected_count()),
                import(ImportAction::Next),
                true,
                wizard.can_next(),
                cx,
            ),
        ]
    }

    fn source_body(wizard: &ImportWizard, tones: Tones, cx: &mut Context<WorkspaceView>) -> Div {
        let size = ui_scale(cx).sm;
        let mut body = div().flex().flex_col().gap(px(SPACE[3])).child(note(
            "Pick the tool to take connections from. Nothing is changed until you confirm.",
            tones,
            cx,
        ));
        if wizard.scanning() {
            return body.child(busy_line(SCANNING_TEXT, tones, cx));
        }
        let mut list = list_column("import-options");
        for (index, option) in wizard.options().into_iter().enumerate() {
            let state = RowState {
                selected: index == wizard.cursor(),
                focused: true,
                disabled: false,
            };
            let place = match &option.path {
                Some(path) => wizard.display_path(path),
                None => NOT_FOUND_TEXT.to_owned(),
            };
            list = list.child(
                list_row(ElementId::from(("import-option", index)), state, cx)
                    .text_size(px(size))
                    .child(icon(AppIcon::Database, tones.muted))
                    .child(div().w(px(LABEL_WIDTH)).flex_none().child(option.label))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(tones.muted)
                            .child(place),
                    )
                    .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                        view.dispatch_connection(import(ImportAction::SetCursor(index)), cx);
                    })),
            );
        }
        body = body.child(list);
        if wizard.loading() {
            body = body.child(busy_line("Reading the file...", tones, cx));
        }
        if let Some(error) = wizard.error() {
            body = body.child(
                div()
                    .flex()
                    .flex_row()
                    .items_start()
                    .gap(px(SPACE[3]))
                    .text_size(px(size))
                    .text_color(tones.danger)
                    .child(icon(AppIcon::CircleAlert, tones.danger))
                    .child(div().flex_1().min_w_0().child(error.to_owned())),
            );
        }
        body
    }

    fn badge_color(badge: EntryBadge, tones: Tones) -> Hsla {
        match badge {
            EntryBadge::Add | EntryBadge::Rename => tones.success,
            EntryBadge::Duplicate => tones.muted,
            EntryBadge::Skipped => tones.danger,
        }
    }

    fn preview_body(wizard: &ImportWizard, tones: Tones, cx: &mut Context<WorkspaceView>) -> Div {
        let size = ui_scale(cx).sm;
        let mut body = div().flex().flex_col().gap(px(SPACE[3]));
        if let Some(label) = wizard.file_label() {
            body = body.child(note(label, tones, cx));
        }
        for notice in wizard.notices() {
            body = body.child(
                div()
                    .flex()
                    .flex_row()
                    .items_start()
                    .gap(px(SPACE[3]))
                    .text_size(px(ui_scale(cx).xs))
                    .text_color(tones.muted)
                    .child(icon(AppIcon::Info, tones.muted))
                    .child(div().flex_1().min_w_0().child(notice.clone())),
            );
        }
        let rows = wizard.rows();
        let mut list = list_column("import-entries");
        for (position, row) in rows.iter().enumerate() {
            let state = RowState {
                selected: position == wizard.cursor(),
                focused: true,
                disabled: !row.selectable,
            };
            let mark = if row.selected {
                icon(AppIcon::Check, tones.success)
            } else {
                icon(AppIcon::Check, gpui::transparent_black())
            };
            let name = if row.group.is_empty() {
                row.name.clone()
            } else {
                format!("{} / {}", row.group, row.name)
            };
            let selectable = row.selectable;
            list = list.child(
                list_row(ElementId::from(("import-entry", position)), state, cx)
                    .text_size(px(size))
                    .child(mark)
                    .child(div().flex_1().min_w_0().truncate().child(name))
                    .child(
                        div()
                            .flex_none()
                            .text_color(Self::badge_color(row.badge, tones))
                            .child(row.badge.label()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .truncate()
                            .text_size(px(ui_scale(cx).xs))
                            .text_color(tones.muted)
                            .child(row.detail.clone()),
                    )
                    .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                        view.dispatch_connection(import(ImportAction::SetCursor(position)), cx);
                        if selectable {
                            view.dispatch_connection(import(ImportAction::ToggleEntry), cx);
                        }
                    })),
            );
        }
        body = body.child(list);
        if let Some(row) = rows.get(wizard.cursor()) {
            let mut remarks = div().flex().flex_col().gap(px(SPACE[0]));
            for text in &row.notes {
                remarks = remarks.child(note(text.clone(), tones, cx));
            }
            if !row.unsupported.is_empty() {
                remarks = remarks.child(note(
                    format!("Not imported: {}", row.unsupported.join(", ")),
                    tones,
                    cx,
                ));
            }
            body = body.child(remarks);
        }
        body
    }

    fn secrets_body(
        wizard: &ImportWizard,
        inputs: Option<&DialogInputs>,
        tones: Tones,
        cx: &mut Context<WorkspaceView>,
    ) -> Div {
        let mut fields = div()
            .id("import-secrets")
            .flex()
            .flex_col()
            .gap(px(SPACE[4]))
            .max_h(px(LIST_MAX_H))
            .overflow_y_scroll();
        for row in wizard.secret_rows() {
            let input = inputs
                .and_then(|i| i.secret_input(row.index, row.kind))
                .map(|state| Input::new(state).mask_toggle());
            let content = div()
                .flex()
                .flex_col()
                .gap(px(SPACE[0]))
                .child(note(row.connection.clone(), tones, cx))
                .children(input);
            fields = fields.child(field_row(row.label, content, cx));
        }
        div()
            .flex()
            .flex_col()
            .gap(px(SPACE[3]))
            .child(note(SECRETS_HINT, tones, cx))
            .child(fields)
    }

    fn summary_body(wizard: &ImportWizard, tones: Tones, cx: &mut Context<WorkspaceView>) -> Div {
        let size = ui_scale(cx).sm;
        let mut body = div()
            .flex()
            .flex_col()
            .gap(px(SPACE[3]))
            .text_size(px(size));
        let Some(summary) = wizard.summary() else {
            return body;
        };
        let line = |kind: AppIcon, color: Hsla, text: String| {
            div()
                .flex()
                .flex_row()
                .items_start()
                .gap(px(SPACE[3]))
                .child(icon(kind, color))
                .child(div().flex_1().min_w_0().child(text))
        };
        let count = summary.added.len();
        body = body.child(line(
            AppIcon::Check,
            tones.success,
            format!(
                "{count} connection{} added.",
                if count == 1 { "" } else { "s" }
            ),
        ));
        let mut added = list_column("import-added");
        for name in &summary.added {
            added = added.child(
                div()
                    .px(px(SPACE[3]))
                    .text_color(tones.muted)
                    .child(name.clone()),
            );
        }
        body = body.child(added);
        if summary.passwords_stored + summary.passwords_skipped > 0 {
            body = body.child(line(
                AppIcon::KeyRound,
                tones.muted,
                format!(
                    "{} password{} sent to the OS keychain, {} left empty.",
                    summary.passwords_stored,
                    if summary.passwords_stored == 1 {
                        ""
                    } else {
                        "s"
                    },
                    summary.passwords_skipped
                ),
            ));
        }
        if summary.not_imported > 0 {
            body = body.child(line(
                AppIcon::Info,
                tones.muted,
                format!("{} not imported.", summary.not_imported),
            ));
        }
        for (name, reason) in &summary.failed {
            body = body.child(line(
                AppIcon::TriangleAlert,
                tones.danger,
                format!("{name}: {reason}"),
            ));
        }
        body
    }

    /// The wizard command a key stands for, if the wizard is open and the key is one of its
    /// keys. `typing` is true while a password field has the focus.
    pub(super) fn import_key(
        &self,
        key: &str,
        shift: bool,
        other_modifiers: bool,
        typing: bool,
    ) -> Option<&'static str> {
        self.ws
            .import_wizard()?
            .key_command(key, shift, other_modifiers, typing)
    }

    /// Carries out an `ImportEffect`: each runs off the UI thread and answers the workspace.
    pub(crate) fn apply_import(&mut self, effect: ImportEffect, cx: &mut Context<Self>) {
        match effect {
            ImportEffect::Discover { request } => {
                let done = self.bridge.blocking(move || {
                    let env = HostEnv::current();
                    let home = env.home.clone();
                    (home, discover_all(&env))
                });
                cx.spawn(async move |this, cx| {
                    let Ok((home, found)) = done.await else {
                        return;
                    };
                    this.update(cx, |view, cx| {
                        view.ws.import_discovered(request, home, found);
                        view.settle(cx);
                    })
                    .ok();
                })
                .detach();
            }
            ImportEffect::PickFile { request } => {
                let picked = cx.prompt_for_paths(PathPromptOptions {
                    files: true,
                    directories: false,
                    multiple: false,
                    prompt: Some("Choose the connections file".into()),
                });
                cx.spawn(async move |this, cx| {
                    let path = match picked.await {
                        Ok(Ok(paths)) => paths.and_then(|p| p.into_iter().next()),
                        Ok(Err(err)) => {
                            let _ = this.update(cx, |view, cx| {
                                view.notices
                                    .push(format!("The file dialog could not open: {err}"));
                                cx.notify();
                            });
                            None
                        }
                        Err(_) => None,
                    };
                    this.update(cx, |view, cx| {
                        let effect = view.ws.import_file_chosen(request, path);
                        view.apply(effect, cx);
                        view.settle(cx);
                    })
                    .ok();
                })
                .detach();
            }
            ImportEffect::ReadFile {
                request,
                source,
                path,
            } => {
                let done = self.bridge.blocking(move || load(source, &path));
                cx.spawn(async move |this, cx| {
                    let Ok(result) = done.await else { return };
                    this.update(cx, |view, cx| {
                        view.ws.import_file_read(request, result);
                        view.settle(cx);
                    })
                    .ok();
                })
                .detach();
            }
        }
    }
}
