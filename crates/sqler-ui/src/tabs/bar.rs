// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Headless decisions of the tab bar: the trailing slot of a tab and the context menu entries.

use super::{TabDecor, TabId};
use crate::workspace::WorkspaceAction;

/// What a tab shows at its trailing edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trailing {
    /// A statement is running.
    Spinner,
    /// The close button.
    Close,
    /// The unsaved-changes dot.
    Dirty,
    /// Nothing.
    None,
}

/// The trailing slot of a tab: a running tab shows the spinner; otherwise a hovered or active tab
/// shows the close button, a dirty one the dot, and any other nothing.
#[must_use]
pub fn trailing(decor: &TabDecor, hovered: bool, active: bool) -> Trailing {
    if decor.running {
        Trailing::Spinner
    } else if hovered || active {
        Trailing::Close
    } else if decor.dirty {
        Trailing::Dirty
    } else {
        Trailing::None
    }
}

/// An entry of the tab context menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuEntry {
    /// Close the tab.
    Close,
    /// Close the other tabs of its group.
    CloseOthers,
    /// Close the tabs to its right.
    CloseToRight,
    /// Close every tab.
    CloseAll,
    /// Keep the tab (turn a preview into a pinned tab).
    Pin,
    /// Move the tab into a new group on the right.
    SplitRight,
    /// Move the tab to the next group.
    MoveToNextGroup,
    /// Copy the title to the clipboard.
    CopyTitle,
}

/// The context menu, in display order.
pub const MENU: [MenuEntry; 8] = [
    MenuEntry::Close,
    MenuEntry::CloseOthers,
    MenuEntry::CloseToRight,
    MenuEntry::CloseAll,
    MenuEntry::Pin,
    MenuEntry::SplitRight,
    MenuEntry::MoveToNextGroup,
    MenuEntry::CopyTitle,
];

impl MenuEntry {
    /// The menu label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Close => "Close",
            Self::CloseOthers => "Close Others",
            Self::CloseToRight => "Close to the Right",
            Self::CloseAll => "Close All",
            Self::Pin => "Pin",
            Self::SplitRight => "Split Right",
            Self::MoveToNextGroup => "Move to Next Group",
            Self::CopyTitle => "Copy Title",
        }
    }

    /// The actions this entry dispatches for tab `id`, in order. Actions that work on the active
    /// tab activate it first. `CopyTitle` dispatches none (the view copies the title).
    #[must_use]
    pub fn actions(self, id: TabId) -> Vec<WorkspaceAction> {
        let on_active = |action| vec![WorkspaceAction::ActivateTab(id), action];
        match self {
            Self::Close => vec![WorkspaceAction::CloseTab(id)],
            Self::CloseOthers => vec![WorkspaceAction::CloseOtherTabs(id)],
            Self::CloseToRight => vec![WorkspaceAction::CloseTabsToRight(id)],
            Self::CloseAll => on_active(WorkspaceAction::CloseAllTabs),
            Self::Pin => vec![WorkspaceAction::PinTab(id)],
            Self::SplitRight => on_active(WorkspaceAction::SplitRight),
            Self::MoveToNextGroup => on_active(WorkspaceAction::MoveActiveTabToNextGroup),
            Self::CopyTitle => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tabs::{TabHeader, TabKind};

    fn decor(dirty: bool, running: bool) -> TabDecor {
        TabDecor {
            header: TabHeader {
                title: "t".into(),
                accent: None,
                badge: None,
                is_prod: false,
                kind: TabKind::Query,
                preview: false,
            },
            dirty,
            running,
        }
    }

    #[test]
    fn trailing_slot_priority() {
        for (dirty, running, hovered, active, want) in [
            (true, true, true, true, Trailing::Spinner),
            (false, true, false, false, Trailing::Spinner),
            (true, false, true, false, Trailing::Close),
            (true, false, false, true, Trailing::Close),
            (false, false, true, false, Trailing::Close),
            (true, false, false, false, Trailing::Dirty),
            (false, false, false, false, Trailing::None),
        ] {
            assert_eq!(
                trailing(&decor(dirty, running), hovered, active),
                want,
                "dirty={dirty} running={running} hovered={hovered} active={active}"
            );
        }
    }

    #[test]
    fn menu_entries_map_to_actions() {
        use WorkspaceAction as A;
        // `TabId` has no public constructor; take one from a strip.
        let mut strip = crate::tabs::TabStrip::default();
        let id = strip.open(
            crate::tabs::TabConnection {
                name: "dev".into(),
                driver: sqler_driver_api::sqler_core::DriverId::new("sqlite").unwrap(),
                color: None,
                environment: None,
            },
            TabKind::Query,
        );
        let want: [(MenuEntry, Vec<A>); 8] = [
            (MenuEntry::Close, vec![A::CloseTab(id)]),
            (MenuEntry::CloseOthers, vec![A::CloseOtherTabs(id)]),
            (MenuEntry::CloseToRight, vec![A::CloseTabsToRight(id)]),
            (
                MenuEntry::CloseAll,
                vec![A::ActivateTab(id), A::CloseAllTabs],
            ),
            (MenuEntry::Pin, vec![A::PinTab(id)]),
            (
                MenuEntry::SplitRight,
                vec![A::ActivateTab(id), A::SplitRight],
            ),
            (
                MenuEntry::MoveToNextGroup,
                vec![A::ActivateTab(id), A::MoveActiveTabToNextGroup],
            ),
            (MenuEntry::CopyTitle, vec![]),
        ];
        assert_eq!(MENU.len(), want.len());
        for (entry, (w_entry, actions)) in MENU.into_iter().zip(want) {
            assert_eq!(entry, w_entry);
            assert_eq!(entry.actions(id), actions, "{}", entry.label());
        }
    }
}
