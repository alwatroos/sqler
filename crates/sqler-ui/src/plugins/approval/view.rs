// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The approval dialog of a plugin write (AC6): the plugin (name and id), the connection and the exact statement,
//! with Deny as the default focus. Nothing runs until the user clicks Approve with the pointer: both Approve
//! buttons are out of the tab order and a click that came from the keyboard is ignored (`pointer_click`), like the
//! grant dialog. On a production connection the second confirmation is the query editor's production dialog, drawn
//! from the queue's run gate.

use std::rc::Rc;

use gpui::{
    AnyElement, App, ClickEvent, Context, FocusHandle, InteractiveElement as _, IntoElement as _,
    ParentElement as _, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{ActiveTheme as _, Sizable as _};
use sqler_driver_api::sqler_core::Environment;

use super::{WriteQueue, WriteStage, asker, card_title};
use crate::query::view::dialogs::confirm_panel;
use crate::theme::SPACE;
use crate::theme::icons::AppIcon;
use crate::theme::widgets::env_chip;
use crate::workspace::WorkspaceView;
use crate::workspace::chrome::{
    DIALOG_W_LG, LIST_MAX_H, dialog_footer, dialog_panel, dialog_title, scrim, ui_scale,
};

/// The line under the statement.
const NOTE: &str = "Nothing runs until you approve.";
/// What follows the note on a production connection.
const NOTE_PROD: &str = " A second confirmation follows.";

/// Receives an answer: the ask, its stage and whether it was approved.
pub(crate) type AnswerFn = Rc<dyn Fn(u64, WriteStage, bool, &mut Window, &mut App)>;

/// Whether `event` is a pointer click. Approve accepts nothing else (a keyboard activation of a button reports one
/// click too).
fn pointer_click(event: &ClickEvent) -> bool {
    event.click_count() == 1 && !event.is_keyboard()
}

/// The dialog for the ask the user answers now; `None` when no ask waits.
pub(crate) fn write_overlay(
    view: &WorkspaceView,
    cx: &mut Context<WorkspaceView>,
) -> Option<AnyElement> {
    let this = cx.entity();
    let on_answer: AnswerFn = Rc::new(move |id, stage, approve, _, cx| {
        this.update(cx, |view, cx| {
            view.plugin_write_answer(id, stage, approve, cx);
        });
    });
    write_dialog(
        view.workspace().plugin_writes(),
        &on_answer,
        view.plugin_write_focus(),
        cx,
    )
}

/// The dialog for the current ask of `queue`, with Deny focused through `focus`.
pub(crate) fn write_dialog(
    queue: &WriteQueue,
    on_answer: &AnswerFn,
    focus: &FocusHandle,
    cx: &App,
) -> Option<AnyElement> {
    let ask = queue.current()?;
    let (id, request) = (ask.id, &ask.request);
    match request.stage {
        WriteStage::ProdSecond => {
            let statements = queue.gate_statements()?;
            let (deny, approve) = (Rc::clone(on_answer), Rc::clone(on_answer));
            let cancel = move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                deny(id, WriteStage::ProdSecond, false, window, cx);
            };
            let confirm = move |event: &ClickEvent, window: &mut Window, cx: &mut App| {
                if pointer_click(event) {
                    approve(id, WriteStage::ProdSecond, true, window, cx);
                }
            };
            let intro = format!("{} asks to run this.", asker(request));
            Some(confirm_panel(
                &request.connection,
                statements,
                Some(intro),
                Some(focus),
                true,
                cancel,
                confirm,
                cx,
            ))
        }
        _ => Some(first_dialog(id, request, on_answer, focus, cx)),
    }
}

fn first_dialog(
    id: u64,
    request: &super::PluginWriteRequest,
    on_answer: &AnswerFn,
    focus: &FocusHandle,
    cx: &App,
) -> AnyElement {
    let scale = ui_scale(cx);
    let theme = cx.theme();
    let (muted, code_bg, radius) = (theme.muted_foreground, theme.muted, theme.radius);
    let mono = theme.mono_font_family.clone();
    let mut note = NOTE.to_owned();
    if request.prod {
        note.push_str(NOTE_PROD);
    }
    let mut who = format!("On {}.", request.connection);
    if request.param_count > 0 {
        who = format!("{who} {} bound parameter(s).", request.param_count);
    }
    let sql = div()
        .id("plugin-write-sql")
        .max_h(px(LIST_MAX_H))
        .overflow_y_scroll()
        .p(px(SPACE[3]))
        .rounded(radius)
        .bg(code_bg)
        .font_family(mono)
        .text_size(px(scale.mono))
        .child(request.sql.clone());
    // The wrapper holds the default focus (Deny), so a stray Enter never approves.
    let on_deny = Rc::clone(on_answer);
    let deny = div()
        .child(
            Button::new("plugin-write-deny")
                .label("Deny")
                .outline()
                .small()
                .on_click(move |_: &ClickEvent, window, cx| {
                    on_deny(id, WriteStage::First, false, window, cx);
                }),
        )
        .track_focus(focus);
    let on_approve = Rc::clone(on_answer);
    // Approve is no tab stop and takes pointer clicks only.
    let mut approve = Button::new("plugin-write-approve")
        .label("Approve")
        .primary()
        .small()
        .tab_stop(false)
        .on_click(move |event: &ClickEvent, window, cx| {
            if pointer_click(event) {
                on_approve(id, WriteStage::First, true, window, cx);
            }
        });
    if request.prod {
        approve = approve.danger();
    }
    let mut title = dialog_title(Some(AppIcon::TriangleAlert), card_title(request), cx);
    if request.prod {
        title = title.child(env_chip(Environment::Prod, cx));
    }
    scrim("plugin-write-scrim", cx)
        .child(
            dialog_panel("plugin-write-dialog", DIALOG_W_LG, cx)
                .debug_selector(|| "plugin-write-dialog".to_owned())
                .child(title)
                .child(div().text_size(px(scale.sm)).text_color(muted).child(who))
                .child(sql)
                .child(div().text_size(px(scale.sm)).text_color(muted).child(note))
                .child(dialog_footer(cx).child(deny).child(approve)),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use gpui::{MouseClickEvent, MouseUpEvent, Render, TestAppContext};

    use super::*;
    use crate::plugins::{PluginWriteAsk, PluginWriteRequest};
    use crate::theme::Appearance;
    use crate::theme::apply::apply;

    type Seen = Rc<RefCell<Vec<(WriteStage, bool)>>>;

    /// Draws the dialog of a queue and records every answer; Deny holds the focus, as the workspace arranges.
    struct Host {
        queue: WriteQueue,
        focus: FocusHandle,
        seen: Seen,
        _reply: tokio::sync::oneshot::Receiver<bool>,
    }

    impl Render for Host {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
            let seen = Rc::clone(&self.seen);
            let on_answer: AnswerFn = Rc::new(move |_, stage, approve, _, _| {
                seen.borrow_mut().push((stage, approve));
            });
            let dialog = write_dialog(&self.queue, &on_answer, &self.focus, cx);
            div().tab_group().size_full().children(dialog)
        }
    }

    /// Walks the tab order from the Deny wrapper and presses Enter and Space on every stop, as a keyboard user can.
    fn walk_and_press(cx: &mut gpui::VisualTestContext) {
        for _ in 0..4 {
            for key in ["enter", "space"] {
                let keystroke = gpui::Keystroke::parse(key).unwrap();
                cx.simulate_event(gpui::KeyDownEvent {
                    keystroke: keystroke.clone(),
                    is_held: false,
                    prefer_character_input: false,
                });
                cx.simulate_event(gpui::KeyUpEvent { keystroke });
            }
            cx.update(gpui::Window::focus_next);
            cx.run_until_parked();
        }
    }

    fn open(cx: &mut TestAppContext, stage: WriteStage) -> (&mut gpui::VisualTestContext, Seen) {
        cx.update(gpui_component::init);
        let seen: Seen = Rc::new(RefCell::new(Vec::new()));
        let seen2 = Rc::clone(&seen);
        let (_host, cx) = cx.add_window_view(move |window, cx| {
            let mut queue = WriteQueue::default();
            let (reply, rx) = tokio::sync::oneshot::channel();
            let request = PluginWriteRequest::new(
                "acme-sync".parse().unwrap(),
                "Acme Sync",
                "Orders",
                sqler_driver_api::sqler_core::DriverId::new("sqlite").unwrap(),
                true,
                "delete from orders where id = 4",
                stage,
            );
            queue.push(PluginWriteAsk { request, reply });
            let focus = cx.focus_handle();
            window.focus(&focus, cx);
            Host {
                queue,
                focus,
                seen: seen2,
                _reply: rx,
            }
        });
        cx.update(|_, cx| apply(Appearance::Dark, 14, cx));
        cx.run_until_parked();
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        (cx, seen)
    }

    #[gpui::test]
    fn tab_and_enter_never_approve_the_first_stage(cx: &mut TestAppContext) {
        let (cx, seen) = open(cx, WriteStage::First);
        assert!(cx.debug_bounds("plugin-write-dialog").is_some());
        walk_and_press(cx);
        // The keyboard reaches Deny (so the walk is real) and never Approve.
        assert!(seen.borrow().contains(&(WriteStage::First, false)));
        assert!(seen.borrow().iter().all(|(_, approve)| !approve));
    }

    #[gpui::test]
    fn tab_and_enter_never_approve_the_production_stage(cx: &mut TestAppContext) {
        let (cx, seen) = open(cx, WriteStage::ProdSecond);
        assert!(cx.debug_bounds("confirm-dialog").is_some());
        walk_and_press(cx);
        assert!(seen.borrow().contains(&(WriteStage::ProdSecond, false)));
        assert!(seen.borrow().iter().all(|(_, approve)| !approve));
    }

    fn mouse(click_count: usize) -> ClickEvent {
        ClickEvent::Mouse(MouseClickEvent {
            up: MouseUpEvent {
                click_count,
                ..Default::default()
            },
            ..Default::default()
        })
    }

    #[test]
    fn only_a_single_pointer_click_counts() {
        assert!(pointer_click(&mouse(1)));
        assert!(!pointer_click(&mouse(2)));
        assert!(!pointer_click(&ClickEvent::Keyboard(
            gpui::KeyboardClickEvent::default()
        )));
    }
}
