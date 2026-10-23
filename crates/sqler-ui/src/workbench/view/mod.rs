// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The workbench chrome: activity bar, side panel, bottom panel frame, Messages, status bar and
//! welcome view. Views only run commands through `WorkspaceView::run_command`; the decisions
//! live in the headless modules next to this one.

mod activity_bar;
mod panel;
mod side_panel;
mod status_bar;
mod welcome;

use std::collections::HashMap;

use gpui::{App, Context, SharedString, Window};
use gpui_component::ActiveTheme as _;

pub(crate) use activity_bar::{activity_bar, focus_index};
pub(crate) use panel::{bottom_panel, messages_body};
pub(crate) use side_panel::side_panel;
pub(crate) use status_bar::status_bar;
pub(crate) use welcome::welcome;

use crate::command::CommandId;
use crate::palette::OPEN_PALETTE;
use crate::theme::{Appearance, TypeScale, type_scale};
use crate::workspace::WorkspaceView;

/// The commands whose shortcut the chrome shows in tooltips and as key hints.
const HINT_COMMANDS: &[&str] = &[
    "workbench:show-connections",
    "workbench:show-saved-queries",
    "workbench:show-ai",
    "workbench:show-plugins",
    "history:open",
    "settings:open",
    "workbench:toggle-side-panel",
    "workbench:toggle-bottom-panel",
    "connection:new",
    "connection:open",
    "editor:new",
    "query:open-saved",
    OPEN_PALETTE,
];

/// The shortcut text of the commands the chrome shows, built once.
pub(crate) struct Hints {
    by_id: HashMap<&'static str, SharedString>,
}

impl Hints {
    /// Asks `f` for the shortcut of each command the chrome shows; commands without one are
    /// left out.
    pub(crate) fn build(f: impl Fn(&CommandId) -> Option<String>) -> Self {
        let by_id = HINT_COMMANDS
            .iter()
            .filter_map(|id| {
                let text = f(&CommandId::new(id).ok()?)?;
                Some((*id, SharedString::from(text)))
            })
            .collect();
        Self { by_id }
    }

    /// The shortcut of command `id`, if it has one.
    pub(crate) fn get(&self, id: &str) -> Option<SharedString> {
        self.by_id.get(id).cloned()
    }
}

/// Runs the command `id` as if it came from the palette or a key.
pub(crate) fn run_command_id(
    view: &mut WorkspaceView,
    id: &str,
    window: &mut Window,
    cx: &mut Context<WorkspaceView>,
) {
    if let Ok(id) = CommandId::new(id) {
        view.run_command(&id, window, cx);
    }
}

/// The type scale of the current UI font size.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped to 1..=u16::MAX
pub(super) fn scale(cx: &App) -> TypeScale {
    let size: f32 = cx.theme().font_size.into();
    type_scale(size.round().clamp(1., f32::from(u16::MAX)) as u16)
}

/// The look of the current theme.
pub(super) fn appearance(cx: &App) -> Appearance {
    if cx.theme().mode.is_dark() {
        Appearance::Dark
    } else {
        Appearance::Light
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hints_keep_only_commands_with_a_shortcut() {
        let h = Hints::build(|id| (id.as_str() == "connection:new").then(|| "⌘N".to_owned()));
        assert_eq!(h.get("connection:new").as_deref(), Some("⌘N"));
        assert_eq!(h.get("history:open"), None);
        assert_eq!(h.get("unknown"), None);
    }

    #[test]
    fn hint_ids_are_valid() {
        for id in HINT_COMMANDS {
            CommandId::new(id).unwrap();
        }
    }
}
