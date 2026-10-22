// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `WorkbenchLayout`: which panels are shown, how big they are, and the region walk of `F6`.

use sqler_config::{BOTTOM_VIEWS, SIDE_VIEWS, WorkbenchState};

/// Smallest side panel width in logical pixels.
pub const SIDE_MIN: f32 = 180.;
/// Largest side panel width in logical pixels.
pub const SIDE_MAX: f32 = 600.;
/// Default side panel width in logical pixels.
pub const SIDE_DEFAULT: f32 = 260.;
/// Smallest bottom panel height in logical pixels.
pub const PANEL_MIN: f32 = 120.;
/// Default bottom panel height in logical pixels.
pub const PANEL_DEFAULT: f32 = 380.;
/// The bottom panel may take this fraction of the window height.
pub const PANEL_MAX_FRACTION: f32 = 0.7;
/// `SetPanelHeight` clamp ceiling in logical pixels.
pub const PANEL_MAX_ABS: f32 = 4096.;

/// A view of the side panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SideView {
    /// The connection list and object trees.
    #[default]
    Connections,
    /// The saved queries.
    SavedQueries,
    /// The AI view: provider, model and opt-in of the active connection.
    Ai,
}

impl SideView {
    /// The name stored in `workbench.toml`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Connections => SIDE_VIEWS[0],
            Self::SavedQueries => SIDE_VIEWS[1],
            Self::Ai => SIDE_VIEWS[2],
        }
    }

    /// Parses a stored name; `None` when unknown.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "connections" => Some(Self::Connections),
            "saved-queries" => Some(Self::SavedQueries),
            "ai" => Some(Self::Ai),
            _ => None,
        }
    }
}

/// A view of the bottom panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PanelView {
    /// Statement results.
    #[default]
    Results,
    /// One line per statement.
    Messages,
    /// The query history.
    History,
    /// The answers of the AI assistant for the active editor tab.
    Assistant,
}

impl PanelView {
    /// The name stored in `workbench.toml`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Results => BOTTOM_VIEWS[0],
            Self::Messages => BOTTOM_VIEWS[1],
            Self::History => BOTTOM_VIEWS[2],
            Self::Assistant => BOTTOM_VIEWS[3],
        }
    }

    /// Parses a stored name; `None` when unknown.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "results" => Some(Self::Results),
            "messages" => Some(Self::Messages),
            "history" => Some(Self::History),
            "assistant" => Some(Self::Assistant),
            _ => None,
        }
    }
}

/// A focus region of the workbench, in `F6` order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Region {
    /// The activity bar.
    ActivityBar,
    /// The side panel.
    SidePanel,
    /// The editor groups.
    Editor,
    /// The bottom panel.
    Panel,
    /// The status bar.
    StatusBar,
}

/// A change to the layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkbenchAction {
    /// Show or hide the side panel.
    ToggleSidePanel,
    /// Show a side view and reveal the panel.
    ShowSideView(SideView),
    /// Show or hide the bottom panel.
    ToggleBottomPanel,
    /// Show a bottom view and reveal the panel.
    ShowPanelView(PanelView),
    /// Set the side panel width, clamped.
    SetSideWidth(u16),
    /// Set the bottom panel height, clamped.
    SetPanelHeight(u16),
    /// Move focus to the next region (the view tracks the region).
    FocusNextRegion,
    /// Move focus to the previous region (the view tracks the region).
    FocusPreviousRegion,
    /// Restore the default layout.
    ResetLayout,
}

/// Panel visibility, sizes and views. Plain data; the shell renders from it.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkbenchLayout {
    side_visible: bool,
    side_width: f32,
    side_view: SideView,
    panel_visible: bool,
    panel_height: f32,
    panel_view: PanelView,
    generation: u64,
}

fn clamp_size(v: f32, min: f32, max: f32, default: f32) -> f32 {
    if v.is_finite() {
        v.clamp(min, max)
    } else {
        default
    }
}

impl WorkbenchLayout {
    /// Builds the layout from the stored state, clamping sizes and mapping unknown views to
    /// the default view.
    #[must_use]
    pub fn from_state(s: &WorkbenchState) -> Self {
        Self {
            side_visible: s.side_panel.visible,
            side_width: clamp_size(s.side_panel.size, SIDE_MIN, SIDE_MAX, SIDE_DEFAULT),
            side_view: SideView::parse(&s.side_panel.view).unwrap_or_default(),
            panel_visible: s.bottom_panel.visible,
            panel_height: clamp_size(s.bottom_panel.size, PANEL_MIN, PANEL_MAX_ABS, PANEL_DEFAULT),
            panel_view: PanelView::parse(&s.bottom_panel.view).unwrap_or_default(),
            generation: 0,
        }
    }

    /// Applies an action. Returns `true` and bumps [`generation`](Self::generation) iff a field
    /// changed. The focus actions never change the layout.
    pub fn apply(&mut self, a: WorkbenchAction) -> bool {
        let before = self.snapshot();
        match a {
            WorkbenchAction::ToggleSidePanel => self.side_visible = !self.side_visible,
            WorkbenchAction::ShowSideView(v) => {
                self.side_view = v;
                self.side_visible = true;
            }
            WorkbenchAction::ToggleBottomPanel => self.panel_visible = !self.panel_visible,
            WorkbenchAction::ShowPanelView(v) => self.reveal_panel(v),
            WorkbenchAction::SetSideWidth(w) => {
                self.side_width = f32::from(w).clamp(SIDE_MIN, SIDE_MAX);
            }
            WorkbenchAction::SetPanelHeight(h) => {
                self.panel_height = f32::from(h).clamp(PANEL_MIN, PANEL_MAX_ABS);
            }
            WorkbenchAction::FocusNextRegion | WorkbenchAction::FocusPreviousRegion => {}
            WorkbenchAction::ResetLayout => {
                let generation = self.generation;
                *self = Self::default();
                self.generation = generation;
            }
        }
        let changed = before != self.snapshot();
        if changed {
            self.generation += 1;
        }
        changed
    }

    fn snapshot(&self) -> (bool, f32, SideView, bool, f32, PanelView) {
        (
            self.side_visible,
            self.side_width,
            self.side_view,
            self.panel_visible,
            self.panel_height,
            self.panel_view,
        )
    }

    /// Shows the bottom panel on `v`.
    pub fn reveal_panel(&mut self, v: PanelView) {
        let before = (self.panel_visible, self.panel_view);
        self.panel_visible = true;
        self.panel_view = v;
        if before != (self.panel_visible, self.panel_view) {
            self.generation += 1;
        }
    }

    /// Counts the changes; the view re-reads the layout when it moves.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Whether the side panel is shown.
    #[must_use]
    pub fn side_visible(&self) -> bool {
        self.side_visible
    }

    /// The side panel width in logical pixels.
    #[must_use]
    pub fn side_width(&self) -> f32 {
        self.side_width
    }

    /// The side view.
    #[must_use]
    pub fn side_view(&self) -> SideView {
        self.side_view
    }

    /// Whether the bottom panel is shown.
    #[must_use]
    pub fn panel_visible(&self) -> bool {
        self.panel_visible
    }

    /// The bottom panel height in logical pixels (not limited by the window).
    #[must_use]
    pub fn panel_height(&self) -> f32 {
        self.panel_height
    }

    /// The bottom view.
    #[must_use]
    pub fn panel_view(&self) -> PanelView {
        self.panel_view
    }

    /// The bottom panel height for a window of `window_height`: at least [`PANEL_MIN`], at most
    /// [`PANEL_MAX_FRACTION`] of the window (but never below the minimum).
    #[must_use]
    pub fn panel_height_in(&self, window_height: f32) -> f32 {
        let max = (window_height * PANEL_MAX_FRACTION).max(PANEL_MIN);
        self.panel_height.clamp(PANEL_MIN, max)
    }
}

impl Default for WorkbenchLayout {
    fn default() -> Self {
        Self::from_state(&WorkbenchState::default())
    }
}

/// The region after (or before) `current`, cyclic, skipping hidden panels.
#[must_use]
pub fn next_region(current: Region, layout: &WorkbenchLayout, forward: bool) -> Region {
    const ORDER: [Region; 5] = [
        Region::ActivityBar,
        Region::SidePanel,
        Region::Editor,
        Region::Panel,
        Region::StatusBar,
    ];
    let len = ORDER.len();
    let start = ORDER.iter().position(|r| *r == current).unwrap_or(0);
    let mut i = start;
    for _ in 0..len {
        i = if forward {
            (i + 1) % len
        } else {
            (i + len - 1) % len
        };
        let r = ORDER[i];
        let hidden = (r == Region::SidePanel && !layout.side_visible)
            || (r == Region::Panel && !layout.panel_visible);
        if !hidden {
            return r;
        }
    }
    current
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqler_config::{BOTTOM_PANEL_SIZE_RANGE, PanelState, SIDE_PANEL_SIZE_RANGE};

    #[test]
    fn consts_match_config() {
        assert_eq!(SIDE_MIN, *SIDE_PANEL_SIZE_RANGE.start());
        assert_eq!(SIDE_MAX, *SIDE_PANEL_SIZE_RANGE.end());
        assert_eq!(PANEL_MIN, *BOTTOM_PANEL_SIZE_RANGE.start());
        assert_eq!(PANEL_MAX_ABS, *BOTTOM_PANEL_SIZE_RANGE.end());
        let d = WorkbenchState::default();
        assert_eq!(SIDE_DEFAULT, d.side_panel.size);
        assert_eq!(PANEL_DEFAULT, d.bottom_panel.size);
    }

    #[test]
    fn view_names_round_trip() {
        for (v, s) in [
            (SideView::Connections, "connections"),
            (SideView::SavedQueries, "saved-queries"),
            (SideView::Ai, "ai"),
        ] {
            assert_eq!(v.as_str(), s);
            assert_eq!(SideView::parse(s), Some(v));
        }
        for (v, s) in [
            (PanelView::Results, "results"),
            (PanelView::Messages, "messages"),
            (PanelView::History, "history"),
            (PanelView::Assistant, "assistant"),
        ] {
            assert_eq!(v.as_str(), s);
            assert_eq!(PanelView::parse(s), Some(v));
        }
        assert_eq!(SideView::parse("x"), None);
        assert_eq!(PanelView::parse("Results"), None);
        for s in SIDE_VIEWS {
            assert!(SideView::parse(s).is_some());
        }
        for s in BOTTOM_VIEWS {
            assert!(PanelView::parse(s).is_some());
        }
    }

    #[test]
    fn default_layout() {
        let l = WorkbenchLayout::default();
        assert!(l.side_visible());
        assert_eq!(l.side_width(), 260.);
        assert_eq!(l.side_view(), SideView::Connections);
        assert!(!l.panel_visible());
        assert_eq!(l.panel_height(), 380.);
        assert_eq!(l.panel_view(), PanelView::Results);
        assert_eq!(l.generation(), 0);
    }

    #[test]
    fn from_state_maps_and_clamps() {
        let s = WorkbenchState {
            side_panel: PanelState {
                visible: false,
                size: 10_000.,
                view: "saved-queries".into(),
            },
            bottom_panel: PanelState {
                visible: true,
                size: f32::NAN,
                view: "nope".into(),
            },
            ..WorkbenchState::default()
        };
        let l = WorkbenchLayout::from_state(&s);
        assert!(!l.side_visible());
        assert_eq!(l.side_width(), 600.);
        assert_eq!(l.side_view(), SideView::SavedQueries);
        assert!(l.panel_visible());
        assert_eq!(l.panel_height(), 380.);
        assert_eq!(l.panel_view(), PanelView::Results);
    }

    #[test]
    fn toggles_flip_and_bump_generation() {
        let mut l = WorkbenchLayout::default();
        assert!(l.apply(WorkbenchAction::ToggleSidePanel));
        assert!(!l.side_visible());
        assert_eq!(l.generation(), 1);
        assert!(l.apply(WorkbenchAction::ToggleSidePanel));
        assert!(l.side_visible());
        assert!(l.apply(WorkbenchAction::ToggleBottomPanel));
        assert!(l.panel_visible());
        assert_eq!(l.generation(), 3);
    }

    #[test]
    fn show_views_set_and_reveal() {
        let mut l = WorkbenchLayout::default();
        l.apply(WorkbenchAction::ToggleSidePanel);
        assert!(l.apply(WorkbenchAction::ShowSideView(SideView::SavedQueries)));
        assert!(l.side_visible());
        assert_eq!(l.side_view(), SideView::SavedQueries);
        // Showing the visible view again changes nothing (no toggling).
        assert!(!l.apply(WorkbenchAction::ShowSideView(SideView::SavedQueries)));
        assert!(l.apply(WorkbenchAction::ShowPanelView(PanelView::Messages)));
        assert!(l.panel_visible());
        assert_eq!(l.panel_view(), PanelView::Messages);
        assert!(!l.apply(WorkbenchAction::ShowPanelView(PanelView::Messages)));
    }

    #[test]
    fn sizes_clamp() {
        let mut l = WorkbenchLayout::default();
        assert!(l.apply(WorkbenchAction::SetSideWidth(10)));
        assert_eq!(l.side_width(), 180.);
        assert!(l.apply(WorkbenchAction::SetSideWidth(u16::MAX)));
        assert_eq!(l.side_width(), 600.);
        assert!(l.apply(WorkbenchAction::SetPanelHeight(1)));
        assert_eq!(l.panel_height(), 120.);
        assert!(l.apply(WorkbenchAction::SetPanelHeight(u16::MAX)));
        assert_eq!(l.panel_height(), 4096.);
        let g = l.generation();
        assert!(!l.apply(WorkbenchAction::SetPanelHeight(u16::MAX)));
        assert_eq!(l.generation(), g);
    }

    #[test]
    fn focus_actions_do_not_change() {
        let mut l = WorkbenchLayout::default();
        assert!(!l.apply(WorkbenchAction::FocusNextRegion));
        assert!(!l.apply(WorkbenchAction::FocusPreviousRegion));
        assert_eq!(l.generation(), 0);
    }

    #[test]
    fn reset_restores_defaults() {
        let mut l = WorkbenchLayout::default();
        assert!(!l.apply(WorkbenchAction::ResetLayout));
        l.apply(WorkbenchAction::SetSideWidth(400));
        l.apply(WorkbenchAction::ToggleBottomPanel);
        let g = l.generation();
        assert!(l.apply(WorkbenchAction::ResetLayout));
        assert_eq!(l.side_width(), 260.);
        assert!(!l.panel_visible());
        assert_eq!(l.generation(), g + 1);
    }

    #[test]
    fn reveal_panel() {
        let mut l = WorkbenchLayout::default();
        l.reveal_panel(PanelView::History);
        assert!(l.panel_visible());
        assert_eq!(l.panel_view(), PanelView::History);
        assert_eq!(l.generation(), 1);
        l.reveal_panel(PanelView::History);
        assert_eq!(l.generation(), 1);
    }

    #[test]
    fn panel_height_in_window() {
        let mut l = WorkbenchLayout::default();
        l.apply(WorkbenchAction::SetPanelHeight(1000));
        assert_eq!(l.panel_height_in(800.), 560.);
        assert_eq!(l.panel_height_in(2000.), 1000.);
        assert_eq!(l.panel_height_in(100.), 120.);
        l.apply(WorkbenchAction::SetPanelHeight(130));
        assert_eq!(l.panel_height_in(100.), 120.);
    }

    fn walk(l: &WorkbenchLayout, forward: bool) -> Vec<Region> {
        let mut out = Vec::new();
        let mut r = Region::Editor;
        for _ in 0..5 {
            r = next_region(r, l, forward);
            out.push(r);
        }
        out
    }

    #[test]
    fn next_region_skips_hidden() {
        use Region::{ActivityBar as A, Editor as E, Panel as P, SidePanel as S, StatusBar as B};
        let mut l = WorkbenchLayout::default(); // side shown, panel hidden
        assert_eq!(walk(&l, true), [B, A, S, E, B]);
        assert_eq!(walk(&l, false), [S, A, B, E, S]);
        l.apply(WorkbenchAction::ToggleBottomPanel); // both shown
        assert_eq!(walk(&l, true), [P, B, A, S, E]);
        assert_eq!(walk(&l, false), [S, A, B, P, E]);
        l.apply(WorkbenchAction::ToggleSidePanel); // panel only
        assert_eq!(walk(&l, true), [P, B, A, E, P]);
        assert_eq!(walk(&l, false), [A, B, P, E, A]);
        l.apply(WorkbenchAction::ToggleBottomPanel); // both hidden
        assert_eq!(walk(&l, true), [B, A, E, B, A]);
        assert_eq!(walk(&l, false), [A, B, E, A, B]);
    }

    #[test]
    fn next_region_from_hidden_region() {
        let l = WorkbenchLayout::default();
        // The panel is hidden but may still be the current region (it was just closed).
        assert_eq!(next_region(Region::Panel, &l, true), Region::StatusBar);
        assert_eq!(next_region(Region::Panel, &l, false), Region::Editor);
    }
}
