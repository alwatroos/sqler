// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The activity bar items and the status bar rule: which item is active and what a click runs.

use sqler_driver_api::sqler_core::Environment;

use super::layout::{PanelView, SideView, WorkbenchLayout};
use super::status::StatusBarModel;

/// An item of the activity bar, top to bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActivityItem {
    /// The connection list and object trees.
    Connections,
    /// The saved queries.
    SavedQueries,
    /// The query history (in the bottom panel).
    History,
    /// The plugin manager (at the bottom of the bar, above AI).
    Plugins,
    /// The AI view (at the bottom of the bar, above Settings).
    Ai,
    /// The settings tab (at the bottom of the bar).
    Settings,
}

impl ActivityItem {
    /// The items above the divider, in order.
    pub const TOP: [ActivityItem; 3] = [Self::Connections, Self::SavedQueries, Self::History];

    /// The items below the divider, in order.
    pub const BOTTOM: [ActivityItem; 3] = [Self::Plugins, Self::Ai, Self::Settings];

    /// How many items there are (one focus handle each; `Settings` is the last index).
    pub const COUNT: usize = 6;

    /// The tooltip title and accessible label.
    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            Self::Connections => "Connections",
            Self::SavedQueries => "Saved Queries",
            Self::History => "History",
            Self::Plugins => "Plugins",
            Self::Ai => "AI",
            Self::Settings => "Settings",
        }
    }

    /// The name of the icon in the application's set; the view maps it to the icon.
    #[must_use]
    pub fn icon_name(self) -> &'static str {
        match self {
            Self::Connections => "database",
            Self::SavedQueries => "bookmark",
            Self::History => "clock",
            Self::Plugins => "plug",
            Self::Ai => "sparkles",
            Self::Settings => "settings",
        }
    }

    /// The command that shows this item's view (the one the shortcut hint belongs to).
    #[must_use]
    pub fn command(self) -> &'static str {
        match self {
            Self::Connections => "workbench:show-connections",
            Self::SavedQueries => "workbench:show-saved-queries",
            Self::History => "history:open",
            Self::Plugins => "workbench:show-plugins",
            Self::Ai => "workbench:show-ai",
            Self::Settings => "settings:open",
        }
    }

    /// Whether the item's view is shown. `Settings` is a tab, not a layout state, so the view
    /// decides it; here it is never active.
    #[must_use]
    pub fn is_active(self, layout: &WorkbenchLayout) -> bool {
        match self {
            Self::Connections => {
                layout.side_visible() && layout.side_view() == SideView::Connections
            }
            Self::SavedQueries => {
                layout.side_visible() && layout.side_view() == SideView::SavedQueries
            }
            Self::History => layout.panel_visible() && layout.panel_view() == PanelView::History,
            Self::Plugins => layout.side_visible() && layout.side_view() == SideView::Plugins,
            Self::Ai => layout.side_visible() && layout.side_view() == SideView::Ai,
            Self::Settings => false,
        }
    }

    /// The command a click runs: an active item hides its panel, an inactive one shows its view.
    #[must_use]
    pub fn click_command(self, layout: &WorkbenchLayout) -> &'static str {
        if !self.is_active(layout) {
            return self.command();
        }
        match self {
            Self::Connections | Self::SavedQueries | Self::Plugins | Self::Ai => {
                "workbench:toggle-side-panel"
            }
            Self::History => "workbench:toggle-bottom-panel",
            Self::Settings => self.command(),
        }
    }
}

/// Whether the status bar is a solid block: production connections only.
#[must_use]
pub fn status_solid(model: &StatusBarModel) -> bool {
    model.env == Some(Environment::Prod)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workbench::WorkbenchAction as A;

    fn layout(actions: &[A]) -> WorkbenchLayout {
        let mut l = WorkbenchLayout::default();
        for a in actions {
            l.apply(*a);
        }
        l
    }

    #[test]
    fn default_layout_shows_connections_only() {
        let l = WorkbenchLayout::default();
        assert!(ActivityItem::Connections.is_active(&l));
        assert!(!ActivityItem::SavedQueries.is_active(&l));
        assert!(!ActivityItem::History.is_active(&l));
        assert!(!ActivityItem::Ai.is_active(&l));
        assert!(!ActivityItem::Settings.is_active(&l));
    }

    #[test]
    fn plugins_and_ai_sit_above_settings() {
        assert_eq!(
            ActivityItem::BOTTOM,
            [
                ActivityItem::Plugins,
                ActivityItem::Ai,
                ActivityItem::Settings
            ]
        );
        assert_eq!(ActivityItem::Plugins.title(), "Plugins");
        assert_eq!(ActivityItem::Plugins.icon_name(), "plug");
        assert_eq!(ActivityItem::Plugins.command(), "workbench:show-plugins");
        let plugins = layout(&[A::ShowSideView(SideView::Plugins)]);
        assert!(ActivityItem::Plugins.is_active(&plugins));
        assert!(!ActivityItem::Ai.is_active(&plugins));
        assert_eq!(
            ActivityItem::Plugins.click_command(&plugins),
            "workbench:toggle-side-panel"
        );
        assert_eq!(
            ActivityItem::Plugins.click_command(&WorkbenchLayout::default()),
            "workbench:show-plugins"
        );
        assert_eq!(
            ActivityItem::COUNT,
            ActivityItem::TOP.len() + ActivityItem::BOTTOM.len()
        );
        assert_eq!(ActivityItem::Ai.title(), "AI");
        assert_eq!(ActivityItem::Ai.icon_name(), "sparkles");
        assert_eq!(ActivityItem::Ai.command(), "workbench:show-ai");
        let l = WorkbenchLayout::default();
        assert_eq!(ActivityItem::Ai.click_command(&l), "workbench:show-ai");
        let ai = layout(&[A::ShowSideView(SideView::Ai)]);
        assert!(ActivityItem::Ai.is_active(&ai));
        assert!(!ActivityItem::Connections.is_active(&ai));
        assert_eq!(
            ActivityItem::Ai.click_command(&ai),
            "workbench:toggle-side-panel"
        );
        let hidden = layout(&[A::ShowSideView(SideView::Ai), A::ToggleSidePanel]);
        assert!(!ActivityItem::Ai.is_active(&hidden));
    }

    #[test]
    fn click_table() {
        let l = WorkbenchLayout::default();
        assert_eq!(
            ActivityItem::Connections.click_command(&l),
            "workbench:toggle-side-panel"
        );
        assert_eq!(
            ActivityItem::SavedQueries.click_command(&l),
            "workbench:show-saved-queries"
        );
        assert_eq!(ActivityItem::History.click_command(&l), "history:open");
        assert_eq!(ActivityItem::Settings.click_command(&l), "settings:open");

        let saved = layout(&[A::ShowSideView(SideView::SavedQueries)]);
        assert!(ActivityItem::SavedQueries.is_active(&saved));
        assert!(!ActivityItem::Connections.is_active(&saved));
        assert_eq!(
            ActivityItem::SavedQueries.click_command(&saved),
            "workbench:toggle-side-panel"
        );
        assert_eq!(
            ActivityItem::Connections.click_command(&saved),
            "workbench:show-connections"
        );

        let history = layout(&[A::ShowPanelView(PanelView::History)]);
        assert!(ActivityItem::History.is_active(&history));
        assert_eq!(
            ActivityItem::History.click_command(&history),
            "workbench:toggle-bottom-panel"
        );
        let results = layout(&[A::ShowPanelView(PanelView::Results)]);
        assert!(!ActivityItem::History.is_active(&results));
    }

    #[test]
    fn hidden_side_panel_deactivates_its_items() {
        let l = layout(&[A::ToggleSidePanel]);
        assert!(!ActivityItem::Connections.is_active(&l));
        assert_eq!(
            ActivityItem::Connections.click_command(&l),
            "workbench:show-connections"
        );
    }

    #[test]
    fn every_command_is_registered() {
        let mut r = crate::command::CommandRegistry::new();
        crate::workbench::register_commands(&mut r).unwrap();
        for item in ActivityItem::TOP.into_iter().chain(ActivityItem::BOTTOM) {
            let id = item.command();
            if id.starts_with("workbench:") {
                assert!(
                    r.get(&crate::command::CommandId::new(id).unwrap())
                        .is_some()
                );
            }
        }
    }

    #[test]
    fn solid_only_for_prod() {
        let with = |env| StatusBarModel {
            env,
            ..StatusBarModel::default()
        };
        assert!(status_solid(&with(Some(Environment::Prod))));
        assert!(!status_solid(&with(Some(Environment::Dev))));
        assert!(!status_solid(&with(Some(Environment::Staging))));
        assert!(!status_solid(&with(None)));
    }
}
