// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The permission dialog (ADR-0059): a full-window overlay that draws a [`GrantPrompt`]. It
//! holds no state; the buttons call the handler with [`GrantCommand::Deny`] or
//! [`GrantCommand::Approve`], and the caller (the plugin manager) records the answer.
//!
//! Deny is listed first and Approve second. Approve answers only to a pointer click: it is not a
//! tab stop and a keyboard-originated click on it is ignored (`activates`). Nothing binds a key to
//! Approve; Escape is Deny (`GrantPrompt::key_command`): the overlay container tracks a focus handle
//! owned by the caller, who focuses it when the dialog opens, and answers Escape from there and from
//! the typed-id field. The container is not a tab stop and Approve stays out of the tab order: Tab
//! walks Deny, then the field, and wraps inside the dialog.
//!
//! The one keyboard route to Approve is typing the plugin id into the field and pressing Enter
//! (`GrantPrompt::confirms`): the text must be exactly the id, a held key never counts, and a locked
//! book offers no field.
//!
//! Every permission that needs attention (`CapabilityLine::prominent`) is shown as a chip in an
//! always-visible block, with its host or scope, so a risky permission cannot hide in the list.
//!
//! The summary line and the chips sit outside the scrolling list.

use std::rc::Rc;

use gpui::{
    AnyElement, App, ClickEvent, Div, Entity, FocusHandle, Focusable as _, InteractiveElement as _,
    IntoElement as _, ParentElement as _, Stateful, Styled as _, Window, div, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputState};
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::{ActiveTheme as _, Icon, Sizable as _};
use sqler_config::{KeyChord, Modifiers};
use sqler_plugin_api::CapabilityKind;

use super::GrantCommand;
use super::prompt::{CapabilityLine, GrantPrompt};
use crate::keymap::chord_from_input;
use crate::keymap::input::key_input;
use crate::theme::SPACE;
use crate::theme::icons::AppIcon;
use crate::workspace::chrome::{
    DIALOG_W_MD, ICON_MD, LIST_MAX_H, dialog_footer, dialog_panel, dialog_title, scrim, ui_scale,
};

/// How strongly a prominent row is tinted with the danger colour.
const WARN_TINT: f32 = 0.12;

/// The scroll hint is shown for more lines than this.
const SCROLL_HINT_FROM: usize = 3;

/// Tab order inside the dialog: Deny first, then the typed-id field.
const TAB_DENY: isize = -1;
const TAB_CONFIRM: isize = 0;

/// What the buttons call.
pub(crate) type GrantHandler = Rc<dyn Fn(GrantCommand, &mut Window, &mut App)>;

fn icon_for(kind: CapabilityKind) -> AppIcon {
    match kind {
        CapabilityKind::DbRead | CapabilityKind::DbWrite => AppIcon::Database,
        CapabilityKind::UiPanel => AppIcon::PanelBottom,
        CapabilityKind::AiTool => AppIcon::Bot,
        CapabilityKind::DriverGuest | CapabilityKind::DriverHost => AppIcon::Server,
        CapabilityKind::FsRead => AppIcon::Folder,
        _ => AppIcon::Plug,
    }
}

fn row(index: usize, line: &CapabilityLine, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let scale = ui_scale(cx);
    let (muted, danger, radius) = (theme.muted_foreground, theme.danger, theme.radius);
    let icon = if line.prominent {
        Icon::new(AppIcon::TriangleAlert)
            .size(px(ICON_MD))
            .text_color(danger)
    } else {
        Icon::new(icon_for(line.kind))
            .size(px(ICON_MD))
            .text_color(muted)
    };
    let mut head = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(SPACE[2]))
        .text_size(px(scale.md))
        .child(line.summary);
    if line.is_new {
        head = head.child(
            div()
                .px(px(SPACE[2]))
                .rounded(radius)
                .bg(theme.primary)
                .text_color(theme.primary_foreground)
                .text_size(px(scale.xs))
                .child("New"),
        );
    }
    let mut text = div()
        .flex()
        .flex_col()
        .gap(px(SPACE[1]))
        .min_w_0()
        .child(head);
    if let Some(detail) = &line.detail {
        text = text.child(
            div()
                .font_family(theme.mono_font_family.clone())
                .text_size(px(scale.mono))
                .child(detail.clone()),
        );
    }
    for note in &line.notes {
        text = text.child(
            div()
                .text_size(px(scale.sm))
                .text_color(if line.prominent { danger } else { muted })
                .child(note.clone()),
        );
    }
    let mut frame: Div = div()
        .flex()
        .flex_row()
        .items_start()
        .gap(px(SPACE[3]))
        .p(px(SPACE[3]))
        .rounded(radius)
        .border_1()
        .border_color(theme.border)
        .child(div().flex_none().pt(px(SPACE[1])).child(icon))
        .child(text);
    if line.prominent {
        frame = frame.bg(danger.opacity(WARN_TINT)).border_color(danger);
    }
    frame
        .debug_selector(move || format!("grant-row-{index}"))
        .into_any_element()
}

/// Whether a click runs `command`. Approve needs a pointer click: a keyboard-originated click
/// (Enter or Space on a focused button) never approves. Deny accepts both.
fn activates(command: GrantCommand, event: &ClickEvent) -> bool {
    event.click_count() == 1 && (command != GrantCommand::Approve || !event.is_keyboard())
}

fn button(id: &'static str, label: &'static str, command: GrantCommand, on: &GrantHandler) -> Div {
    let on = Rc::clone(on);
    let b = Button::new(id)
        .label(label)
        .small()
        .on_click(move |event: &ClickEvent, window, cx| {
            if activates(command, event) {
                on(command, window, cx);
            }
        });
    // Deny is the first tab stop of the dialog and the typed-id field the second; Approve is no tab stop.
    let b = match command {
        GrantCommand::Approve => b.primary().tab_stop(false),
        _ => b.outline().tab_index(TAB_DENY),
    };
    div().debug_selector(move || id.to_owned()).child(b)
}

/// The always-visible summary line and the compact text of every prominent permission, so none of
/// them depends on scrolling.
fn summary_and_attention(prompt: &GrantPrompt, cx: &App) -> (String, Stateful<Div>) {
    let theme = cx.theme();
    let scale = ui_scale(cx);
    let (danger, radius) = (theme.danger, theme.radius);
    let mut summary = prompt.summary();
    if prompt.lines().len() > SCROLL_HINT_FROM {
        summary.push_str(". The list below scrolls.");
    }
    let mut attention = div()
        .id("grant-attention")
        .flex()
        .flex_row()
        .flex_wrap()
        .gap(px(SPACE[2]))
        .font_family(theme.mono_font_family.clone())
        .text_size(px(scale.mono))
        .text_color(danger);
    for line in prompt.lines().iter().filter(|l| l.prominent) {
        attention = attention.child(
            div()
                .px(px(SPACE[2]))
                .rounded(radius)
                .border_1()
                .border_color(danger)
                .child(line.capability.clone()),
        );
    }

    (summary, attention)
}

/// The banner that says why Approve is not offered.
fn problem_banner(problem: &str, cx: &App) -> Div {
    let theme = cx.theme();
    let scale = ui_scale(cx);
    let danger = theme.danger;
    div()
        .flex()
        .flex_row()
        .items_start()
        .gap(px(SPACE[3]))
        .p(px(SPACE[3]))
        .rounded(theme.radius)
        .border_1()
        .border_color(danger)
        .text_size(px(scale.sm))
        .text_color(danger)
        .child(Icon::new(AppIcon::TriangleAlert).size(px(ICON_MD)))
        .child(format!(
            "Permissions cannot be approved right now: {problem}"
        ))
}

/// The keyboard route to Approve: a label that names the id to type and the field for it.
fn confirm_field(prompt: &GrantPrompt, confirm: &Entity<InputState>, cx: &App) -> Div {
    let theme = cx.theme();
    let scale = ui_scale(cx);
    let id = prompt.plugin_id();
    div()
        .flex()
        .flex_col()
        .gap(px(SPACE[1]))
        .debug_selector(|| "grant-confirm".to_owned())
        .child(
            div()
                .text_size(px(scale.sm))
                .text_color(theme.muted_foreground)
                .child(format!(
                    "To approve with the keyboard, type {id} here and press Enter."
                )),
        )
        .child(
            Input::new(confirm)
                .small()
                .tab_index(TAB_CONFIRM)
                .aria_label(format!("Type {id} to approve")),
        )
}

/// Whether `chord` is a plain Enter.
fn is_plain_enter(chord: &KeyChord) -> bool {
    chord.key() == "enter" && chord.modifiers() == Modifiers::default()
}

/// `Some(true)` for Tab, `Some(false)` for Shift+Tab, `None` for a Tab with other modifiers.
fn tab_direction(chord: &KeyChord) -> Option<bool> {
    let shift = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    match chord.modifiers() {
        m if m == Modifiers::default() => Some(true),
        m if m == shift => Some(false),
        _ => None,
    }
}

/// Moves the focus to the next (or previous) tab stop and keeps it inside the dialog: past the last stop it wraps to
/// Deny (the first), before the first it wraps to the field (the last), or to Deny when there is no field.
fn tab_within(
    container: &FocusHandle,
    confirm: &Entity<InputState>,
    has_field: bool,
    forward: bool,
    window: &mut Window,
    cx: &mut App,
) {
    if forward {
        window.focus_next(cx);
    } else {
        window.focus_prev(cx);
    }
    if container.contains_focused(window, cx) && !container.is_focused(window) {
        return;
    }
    // Left the dialog (or landed on its container): start again from the container.
    if !forward && has_field {
        window.focus(&confirm.focus_handle(cx), cx);
        return;
    }
    window.focus(container, cx);
    window.focus_next(cx);
}

/// Draws the dialog for `prompt`. The Approve button is left out when the book is unavailable.
/// `focus` belongs to the caller, who focuses it when the dialog opens; Escape pressed anywhere
/// inside the dialog (also from the field) is Deny.
///
/// `confirm` is the typed-id field, owned by the caller (and emptied when a dialog opens). Enter in the field
/// approves only when its text is exactly the plugin id ([`GrantPrompt::confirms`]); Approve answers no other key.
#[allow(clippy::needless_pass_by_value)] // the handler type is the contract; a clone is cheap
#[allow(clippy::too_many_lines)] // one declarative element tree
pub(crate) fn grant_overlay(
    prompt: &GrantPrompt,
    on: GrantHandler,
    focus: &FocusHandle,
    confirm: &Entity<InputState>,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let scale = ui_scale(cx);
    let muted = theme.muted_foreground;

    let mut body = div()
        .flex()
        .flex_col()
        .gap(px(SPACE[2]))
        .text_size(px(scale.sm));
    body = body.child(div().text_color(muted).child(format!(
        "{} {} · {}",
        prompt.name(),
        prompt.version(),
        prompt.plugin_id()
    )));
    if let Some(description) = prompt.description() {
        body = body.child(
            div()
                .text_size(px(scale.md))
                .text_color(theme.popover_foreground)
                .child(description.to_owned()),
        );
    }
    if !prompt.contributions().is_empty() {
        body = body.child(
            div()
                .text_color(muted)
                .child(format!("Adds {}.", prompt.contributions().join(", "))),
        );
    }
    if prompt.is_reprompt() {
        body = body.child(div().text_color(muted).child(
            "This plugin changed since you last decided. Permissions it did not have before are marked New.",
        ));
    }

    let mut list = div()
        .id("grant-list")
        .debug_selector(|| "grant-list".to_owned())
        .max_h(px(LIST_MAX_H))
        .flex()
        .flex_col()
        .gap(px(SPACE[2]));
    for (i, line) in prompt.lines().iter().enumerate() {
        list = list.child(row(i, line, cx));
    }
    let list = list.overflow_y_scrollbar();

    let (summary, attention) = summary_and_attention(prompt, cx);

    let mut footer = dialog_footer(cx).child(button("grant-deny", "Deny", GrantCommand::Deny, &on));
    if prompt.can_approve() {
        footer = footer.child(button(
            "grant-approve",
            "Approve",
            GrantCommand::Approve,
            &on,
        ));
    }

    let mut panel = dialog_panel("grant-dialog", DIALOG_W_MD, cx)
        .debug_selector(|| "grant-dialog".to_owned())
        .child(dialog_title(
            Some(AppIcon::Plug),
            prompt.title().to_owned(),
            cx,
        ))
        .child(body);
    if let Some(problem) = prompt.problem() {
        panel = panel.child(problem_banner(problem, cx));
    }
    panel = panel
        .child(
            div()
                .text_size(px(scale.sm))
                .text_color(muted)
                .child("This plugin asks to be allowed to:"),
        )
        .child(
            div()
                .debug_selector(|| "grant-summary".to_owned())
                .text_size(px(scale.md))
                .child(summary),
        );
    if prompt.attention_count() > 0 {
        panel = panel.child(attention.debug_selector(|| "grant-attention".to_owned()));
    }
    panel = panel.child(list);
    if prompt.can_approve() {
        panel = panel.child(confirm_field(prompt, confirm, cx));
    }
    panel = panel.child(footer).tab_group();

    let answer = Rc::clone(&on);
    let (prompt, confirm, container) = (prompt.clone(), confirm.clone(), focus.clone());
    // Capture phase: Escape, Tab and Enter are decided here, before the field's own actions see them.
    scrim("grant-scrim", cx)
        .track_focus(focus)
        .capture_key_down(move |event, window, cx| {
            let Some(chord) = chord_from_input(&key_input(&event.keystroke)) else {
                return;
            };
            if let Some(command) = GrantPrompt::key_command(&chord) {
                cx.stop_propagation();
                answer(command, window, cx);
                return;
            }
            if chord.key() == "tab"
                && let Some(forward) = tab_direction(&chord)
            {
                // The dialog is modal: Tab walks Deny, the field and wraps; it never reaches the window behind.
                cx.stop_propagation();
                tab_within(
                    &container,
                    &confirm,
                    prompt.can_approve(),
                    forward,
                    window,
                    cx,
                );
                return;
            }
            // Enter in the field is the only keyboard route to Approve, and only for the exact id. A held key never
            // approves.
            if is_plain_enter(&chord) && confirm.focus_handle(cx).is_focused(window) {
                cx.stop_propagation();
                if !event.is_held && prompt.confirms(confirm.read(cx).value().as_ref()) {
                    answer(GrantCommand::Approve, window, cx);
                }
            }
        })
        .child(panel)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use gpui::{AppContext as _, TestAppContext};
    use gpui::{Context, Render};

    use super::*;
    use crate::plugins::grant::sample_prompt;
    use crate::theme::Appearance;
    use crate::theme::apply::apply;

    const ROWS: [&str; 5] = [
        "grant-row-0",
        "grant-row-1",
        "grant-row-2",
        "grant-row-3",
        "grant-row-4",
    ];

    /// Draws the overlay and records every answer; focuses the overlay like the caller must.
    struct Host {
        prompt: GrantPrompt,
        focus: FocusHandle,
        confirm: Entity<InputState>,
        seen: Rc<RefCell<Vec<GrantCommand>>>,
    }

    impl Host {
        fn new(
            prompt: GrantPrompt,
            seen: Rc<RefCell<Vec<GrantCommand>>>,
            window: &mut Window,
            cx: &mut Context<Self>,
        ) -> Self {
            Self {
                prompt,
                focus: cx.focus_handle(),
                confirm: cx.new(|cx| InputState::new(window, cx)),
                seen,
            }
        }
    }

    impl Render for Host {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
            let seen = Rc::clone(&self.seen);
            grant_overlay(
                &self.prompt,
                Rc::new(move |c, _, _| seen.borrow_mut().push(c)),
                &self.focus,
                &self.confirm,
                cx,
            )
        }
    }

    fn open(
        cx: &mut TestAppContext,
        prompt: GrantPrompt,
    ) -> (
        gpui::Entity<Host>,
        &mut gpui::VisualTestContext,
        Rc<RefCell<Vec<GrantCommand>>>,
    ) {
        cx.update(gpui_component::init);
        let seen = Rc::new(RefCell::new(Vec::new()));
        let seen2 = Rc::clone(&seen);
        let (host, cx) = cx.add_window_view(move |window, cx| Host::new(prompt, seen2, window, cx));
        cx.update(|window, cx| {
            apply(Appearance::Dark, 14, cx);
            let focus = host.read(cx).focus.clone();
            window.focus(&focus, cx);
        });
        cx.run_until_parked();
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        (host, cx, seen)
    }

    fn has_bounds(cx: &mut gpui::VisualTestContext, selector: &'static str) -> bool {
        cx.debug_bounds(selector)
            .is_some_and(|b| b.size.width > px(0.) && b.size.height > px(0.))
    }

    #[gpui::test]
    fn the_dialog_renders_in_both_themes(cx: &mut TestAppContext) {
        let prompt = sample_prompt();
        assert_eq!(prompt.lines().len(), ROWS.len());
        let (_host, cx, _) = open(cx, prompt);
        for look in [Appearance::Dark, Appearance::Light] {
            cx.update(|_, cx| apply(look, 14, cx));
            cx.run_until_parked();
            cx.update(|window, _| window.refresh());
            cx.run_until_parked();
            for selector in ["grant-dialog", "grant-approve", "grant-deny"] {
                assert!(has_bounds(cx, selector), "{selector} in {look:?}");
            }
            for selector in ROWS {
                assert!(has_bounds(cx, selector), "{selector} in {look:?}");
            }
        }
    }

    #[gpui::test]
    fn a_problem_removes_approve(cx: &mut TestAppContext) {
        let prompt = sample_prompt().with_problem("the file is damaged");
        let (_host, cx, _) = open(cx, prompt);
        assert!(has_bounds(cx, "grant-deny"));
        assert!(cx.debug_bounds("grant-approve").is_none());
    }

    fn many(attention: usize, total: usize) -> GrantPrompt {
        use crate::plugins::grant::book::GrantStatus;
        use crate::plugins::grant::prompt::{fixture_manifest, fixture_prompt};
        let mut caps: Vec<String> = (0..attention)
            .map(|i| format!("fs.read:home/Library/app{i}"))
            .collect();
        caps.push("db.read".to_owned());
        caps.push("db.write".to_owned());
        let mut i = 0;
        while caps.len() < total {
            caps.push(format!("fs.read:data/d{i}"));
            i += 1;
        }
        let refs: Vec<&str> = caps.iter().map(String::as_str).collect();
        let m = fixture_manifest("acme-sync", "1.0.0", &refs, true);
        fixture_prompt(&m, &GrantStatus::NeedsPrompt { previous: None })
    }

    #[gpui::test]
    fn many_attention_permissions_stay_visible_outside_the_scroll_area(cx: &mut TestAppContext) {
        for (attention, total) in [(8, 10), (10, 32)] {
            let prompt = many(attention, total);
            assert_eq!(prompt.lines().len(), total);
            // Everything needs attention.
            assert_eq!(prompt.attention_count(), total);
            let (_host, cx, _) = open(cx, prompt);
            for selector in [
                "grant-summary",
                "grant-attention",
                "grant-list",
                "grant-approve",
                "grant-deny",
            ] {
                assert!(has_bounds(cx, selector), "{selector} for {total}");
            }
            // The dialog stays inside the window: Approve is below the summary and reachable.
            let summary = cx.debug_bounds("grant-summary").unwrap();
            let approve = cx.debug_bounds("grant-approve").unwrap();
            assert!(approve.origin.y > summary.origin.y);
            let window = cx.update(|window, _| window.viewport_size());
            assert!(
                approve.origin.y + approve.size.height <= window.height,
                "{total}"
            );
        }
    }

    #[test]
    fn a_keyboard_click_never_approves() {
        use gpui::{MouseClickEvent, MouseUpEvent};
        let keyboard = ClickEvent::Keyboard(gpui::KeyboardClickEvent::default());
        assert!(!activates(GrantCommand::Approve, &keyboard));
        assert!(activates(GrantCommand::Deny, &keyboard));
        let mouse = ClickEvent::Mouse(MouseClickEvent {
            up: MouseUpEvent {
                click_count: 1,
                ..Default::default()
            },
            ..Default::default()
        });
        assert!(activates(GrantCommand::Approve, &mouse));
        assert!(activates(GrantCommand::Deny, &mouse));
    }

    #[gpui::test]
    fn tab_and_enter_never_approve(cx: &mut TestAppContext) {
        let (_host, cx, seen) = open(cx, sample_prompt());
        cx.simulate_keystrokes("tab tab tab enter space");
        cx.run_until_parked();
        assert!(!seen.borrow().contains(&GrantCommand::Approve));
    }

    fn typed(host: &gpui::Entity<Host>, cx: &mut gpui::VisualTestContext, text: &str) {
        let confirm = host.read_with(cx, |h, _| h.confirm.clone());
        cx.update(|window, cx| {
            confirm.update(cx, |input, cx| input.set_value(text.to_owned(), window, cx));
        });
    }

    fn confirm_focused(host: &gpui::Entity<Host>, cx: &mut gpui::VisualTestContext) -> bool {
        let confirm = host.read_with(cx, |h, _| h.confirm.clone());
        cx.update(|window, cx| confirm.focus_handle(cx).is_focused(window))
    }

    /// Presses Tab and draws, so the element that took the focus has registered its key listeners.
    fn tab(host: &gpui::Entity<Host>, cx: &mut gpui::VisualTestContext) {
        cx.simulate_keystrokes("tab");
        host.update(cx, |_, cx| cx.notify());
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
    }

    #[gpui::test]
    fn the_dialog_has_the_focus_and_deny_is_the_first_tab_stop(cx: &mut TestAppContext) {
        let (host, cx, seen) = open(cx, sample_prompt());
        let focus = host.read_with(cx, |h, _| h.focus.clone());
        assert!(cx.update(|window, _| focus.is_focused(window)));
        // The first Tab lands inside the dialog on a tab stop that is neither the container nor the field (Deny); Enter
        // there never approves.
        tab(&host, cx);
        assert!(!confirm_focused(&host, cx));
        assert!(!cx.update(|window, _| focus.is_focused(window)));
        assert!(cx.update(|window, cx| focus.contains_focused(window, cx)));
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert!(!seen.borrow().contains(&GrantCommand::Approve));
    }

    #[gpui::test]
    fn the_second_tab_stop_is_the_typed_id_field_and_never_approve(cx: &mut TestAppContext) {
        let (host, cx, seen) = open(cx, sample_prompt());
        tab(&host, cx);
        tab(&host, cx);
        assert!(confirm_focused(&host, cx));
        // Empty field: Enter is a no-op.
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert!(seen.borrow().is_empty());
        // A third Tab wraps to Deny and the walk never reaches Approve (it is no tab stop).
        for _ in 0..4 {
            tab(&host, cx);
            cx.simulate_keystrokes("space");
        }
        cx.run_until_parked();
        assert!(!seen.borrow().contains(&GrantCommand::Approve));
    }

    #[gpui::test]
    fn tab_wraps_inside_the_dialog_in_both_directions(cx: &mut TestAppContext) {
        let (host, cx, _) = open(cx, sample_prompt());
        tab(&host, cx);
        tab(&host, cx);
        assert!(confirm_focused(&host, cx));
        tab(&host, cx);
        assert!(!confirm_focused(&host, cx), "wrapped to Deny");
        let focus = host.read_with(cx, |h, _| h.focus.clone());
        assert!(cx.update(|window, cx| focus.contains_focused(window, cx)));
        cx.simulate_keystrokes("shift-tab");
        assert!(
            confirm_focused(&host, cx),
            "Shift+Tab from Deny wraps to the field"
        );
    }

    #[gpui::test]
    fn the_exact_id_and_enter_approve_and_nothing_else_does(cx: &mut TestAppContext) {
        let (host, cx, seen) = open(cx, sample_prompt());
        tab(&host, cx);
        tab(&host, cx);
        for wrong in [
            "acme",
            "Acme-Sync",
            "acme-sync ",
            " acme-sync",
            "acme-sync2",
        ] {
            typed(&host, cx, wrong);
            cx.simulate_keystrokes("enter");
            cx.run_until_parked();
            assert!(seen.borrow().is_empty(), "{wrong:?}");
        }
        typed(&host, cx, "acme-sync");
        cx.simulate_keystrokes("shift-enter");
        cx.simulate_keystrokes("cmd-enter");
        cx.run_until_parked();
        assert!(seen.borrow().is_empty(), "a modified Enter does nothing");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(*seen.borrow(), [GrantCommand::Approve]);
    }

    #[gpui::test]
    fn enter_with_the_id_typed_but_the_focus_elsewhere_does_not_approve(cx: &mut TestAppContext) {
        let (host, cx, seen) = open(cx, sample_prompt());
        typed(&host, cx, "acme-sync");
        cx.simulate_keystrokes("enter");
        tab(&host, cx);
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert!(
            !seen.borrow().contains(&GrantCommand::Approve),
            "the id sits in the field but the field is not focused"
        );
    }

    #[gpui::test]
    fn a_locked_book_offers_no_field_and_enter_never_approves(cx: &mut TestAppContext) {
        let (host, cx, seen) = open(cx, sample_prompt().with_problem("damaged"));
        assert!(cx.debug_bounds("grant-confirm").is_none());
        typed(&host, cx, "acme-sync");
        tab(&host, cx);
        tab(&host, cx);
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert!(!seen.borrow().contains(&GrantCommand::Approve));
    }

    #[gpui::test]
    fn escape_denies_from_the_field(cx: &mut TestAppContext) {
        let (host, cx, seen) = open(cx, sample_prompt());
        tab(&host, cx);
        tab(&host, cx);
        assert!(confirm_focused(&host, cx));
        typed(&host, cx, "acme-sync");
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert_eq!(*seen.borrow(), [GrantCommand::Deny]);
    }

    #[gpui::test]
    fn escape_denies(cx: &mut TestAppContext) {
        let (_host, cx, seen) = open(cx, sample_prompt());
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert_eq!(*seen.borrow(), [GrantCommand::Deny]);
    }

    #[gpui::test]
    fn escape_denies_from_a_focused_button_and_other_keys_do_nothing(cx: &mut TestAppContext) {
        let (_host, cx, seen) = open(cx, sample_prompt());
        // Tab moves focus onto Deny (the only tab stop); Escape still denies.
        cx.simulate_keystrokes("a y ctrl-escape shift-escape cmd-enter tab");
        cx.run_until_parked();
        assert!(seen.borrow().is_empty());
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert_eq!(*seen.borrow(), [GrantCommand::Deny]);
    }
}
