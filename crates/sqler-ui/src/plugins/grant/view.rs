// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The permission dialog (ADR-0059): a full-window overlay that draws a [`GrantPrompt`]. It
//! holds no state; the buttons call the handler with [`GrantCommand::Deny`] or
//! [`GrantCommand::Approve`], and the caller (the plugin manager) records the answer.
//!
//! Deny is listed first and Approve second. Approve answers only to a pointer click: it is not a
//! tab stop and a keyboard-originated click on it is ignored (`activates`). Nothing binds a key to
//! Approve; Escape is Deny (`GrantPrompt::key_command`).
//!
//! The summary line and the compact text of every prominent permission sit outside the scrolling
//! list, so a dangerous permission cannot be clipped away while Approve stays visible.

use std::rc::Rc;

use gpui::{
    AnyElement, App, ClickEvent, Div, InteractiveElement as _, IntoElement as _,
    ParentElement as _, Stateful, Styled as _, Window, div, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::{ActiveTheme as _, Icon, Sizable as _};
use sqler_plugin_api::CapabilityKind;

use super::GrantCommand;
use super::prompt::{CapabilityLine, GrantPrompt};
use crate::theme::SPACE;
use crate::theme::icons::AppIcon;
use crate::workspace::chrome::{
    DIALOG_W_MD, ICON_MD, LIST_MAX_H, dialog_footer, dialog_panel, dialog_title, scrim, ui_scale,
};

/// How strongly a prominent row is tinted with the danger colour.
const WARN_TINT: f32 = 0.12;

/// The scroll hint is shown for more lines than this.
const SCROLL_HINT_FROM: usize = 3;

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
    let b = match command {
        GrantCommand::Approve => b.primary().tab_stop(false),
        _ => b.outline(),
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
    if prompt.lines.len() > SCROLL_HINT_FROM {
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
    for line in prompt.lines.iter().filter(|l| l.prominent) {
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

/// Draws the dialog for `prompt`. The Approve button is left out when the book is unavailable.
#[allow(clippy::needless_pass_by_value)] // the handler type is the contract; a clone is cheap
pub(crate) fn grant_overlay(prompt: &GrantPrompt, on: GrantHandler, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let scale = ui_scale(cx);
    let (muted, danger, radius) = (theme.muted_foreground, theme.danger, theme.radius);

    let mut body = div()
        .flex()
        .flex_col()
        .gap(px(SPACE[2]))
        .text_size(px(scale.sm));
    body = body.child(div().text_color(muted).child(format!(
        "{} {} · {}",
        prompt.name, prompt.version, prompt.plugin_id
    )));
    if let Some(description) = &prompt.description {
        body = body.child(
            div()
                .text_size(px(scale.md))
                .text_color(theme.popover_foreground)
                .child(description.clone()),
        );
    }
    if !prompt.contributions.is_empty() {
        body = body.child(
            div()
                .text_color(muted)
                .child(format!("Adds {}.", prompt.contributions.join(", "))),
        );
    }
    if prompt.reprompt {
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
    for (i, line) in prompt.lines.iter().enumerate() {
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
        .child(dialog_title(Some(AppIcon::Plug), prompt.title.clone(), cx))
        .child(body);
    if let Some(problem) = &prompt.problem {
        panel = panel.child(
            div()
                .flex()
                .flex_row()
                .items_start()
                .gap(px(SPACE[3]))
                .p(px(SPACE[3]))
                .rounded(radius)
                .border_1()
                .border_color(danger)
                .text_size(px(scale.sm))
                .text_color(danger)
                .child(Icon::new(AppIcon::TriangleAlert).size(px(ICON_MD)))
                .child(format!(
                    "Permissions cannot be approved right now: {problem}"
                )),
        );
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
    panel = panel.child(list).child(footer);

    scrim("grant-scrim", cx).child(panel).into_any_element()
}

/// The `plugins-grant` scene: the real workspace with the dialog drawn over it.
#[cfg(feature = "screenshots")]
pub(crate) struct GrantSceneHost {
    workspace: gpui::Entity<crate::workspace::WorkspaceView>,
    prompt: GrantPrompt,
}

#[cfg(feature = "screenshots")]
impl GrantSceneHost {
    pub(crate) fn new(
        workspace: gpui::Entity<crate::workspace::WorkspaceView>,
        prompt: GrantPrompt,
    ) -> Self {
        Self { workspace, prompt }
    }
}

#[cfg(feature = "screenshots")]
impl gpui::Render for GrantSceneHost {
    fn render(&mut self, _: &mut Window, cx: &mut gpui::Context<Self>) -> impl gpui::IntoElement {
        div()
            .relative()
            .size_full()
            .child(self.workspace.clone())
            .child(grant_overlay(&self.prompt, Rc::new(|_, _, _| {}), cx))
    }
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;
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

    struct Host(GrantPrompt);

    impl Render for Host {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
            grant_overlay(&self.0, Rc::new(|_, _, _| {}), cx)
        }
    }

    fn has_bounds(cx: &mut gpui::VisualTestContext, selector: &'static str) -> bool {
        cx.debug_bounds(selector)
            .is_some_and(|b| b.size.width > px(0.) && b.size.height > px(0.))
    }

    #[gpui::test]
    fn the_dialog_renders_in_both_themes(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let prompt = sample_prompt();
        assert_eq!(prompt.lines.len(), ROWS.len());
        let (_host, cx) = cx.add_window_view(|_, _| Host(prompt));
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
        cx.update(gpui_component::init);
        let mut prompt = sample_prompt();
        prompt.problem = Some("the file is damaged".into());
        let (_host, cx) = cx.add_window_view(|_, _| Host(prompt));
        cx.update(|_, cx| apply(Appearance::Dark, 14, cx));
        cx.run_until_parked();
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        assert!(has_bounds(cx, "grant-deny"));
        assert!(cx.debug_bounds("grant-approve").is_none());
    }

    fn many(prominent: usize, total: usize) -> GrantPrompt {
        use crate::plugins::grant::book::GrantStatus;
        use crate::plugins::grant::prompt::{fixture_key, fixture_manifest};
        let mut caps: Vec<String> = (0..prominent)
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
        GrantPrompt::new(
            &m,
            fixture_key(&m),
            &GrantStatus::NeedsPrompt { previous: None },
        )
    }

    #[gpui::test]
    fn many_prominent_permissions_stay_visible_outside_the_scroll_area(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        for (prominent, total) in [(8, 10), (10, 32)] {
            let prompt = many(prominent, total);
            assert_eq!(prompt.lines.len(), total);
            assert_eq!(prompt.attention_count(), prominent + 1); // + db.write
            let (_host, cx) = cx.add_window_view(|_, _| Host(prompt));
            cx.update(|_, cx| apply(Appearance::Dark, 14, cx));
            cx.run_until_parked();
            cx.update(|window, _| window.refresh());
            cx.run_until_parked();
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
        use std::cell::RefCell;
        struct Recording(GrantPrompt, Rc<RefCell<Vec<GrantCommand>>>);
        impl Render for Recording {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
                let seen = Rc::clone(&self.1);
                grant_overlay(
                    &self.0,
                    Rc::new(move |c, _, _| seen.borrow_mut().push(c)),
                    cx,
                )
            }
        }
        cx.update(gpui_component::init);
        let seen = Rc::new(RefCell::new(Vec::new()));
        let seen2 = Rc::clone(&seen);
        let (_host, cx) = cx.add_window_view(move |_, _| Recording(sample_prompt(), seen2));
        cx.update(|_, cx| apply(Appearance::Dark, 14, cx));
        cx.run_until_parked();
        cx.simulate_keystrokes("tab tab tab enter space");
        cx.run_until_parked();
        assert!(!seen.borrow().contains(&GrantCommand::Approve));
    }
}
