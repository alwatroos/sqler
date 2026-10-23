// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `PanelSlot`: the headless state of one plugin panel (ADR-0060).
//!
//! A slot is `Loading`, shows a `Tree` or `Failed` with a reason. It never calls the plugin itself: each method takes
//! the time (so tests inject it) and returns the [`Step`] the window should carry out. The rules:
//!
//! - at most one render or event call is in flight;
//! - a refresh that arrives during a call queues one follow-up, however many arrive;
//! - two renders start at least [`RENDER_GAP`] apart (the step is then [`Step::RenderAfter`]);
//! - up to [`EVENT_QUEUE`] events wait for the call in flight, the rest are dropped;
//! - a panel renders only while it is shown; a refresh while hidden marks it stale and the next show renders.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use sqler_plugin_api::{WidgetEvent, WidgetTree};

use super::backend::PanelRender;

/// The shortest time between two renders of one panel.
pub const RENDER_GAP: Duration = Duration::from_millis(100);
/// How many events wait for the call in flight.
pub const EVENT_QUEUE: usize = 8;

/// What the slot shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotView {
    /// Nothing yet.
    Loading,
    /// The tree the plugin drew.
    Tree(WidgetTree),
    /// The reason the panel cannot be shown.
    Failed(String),
}

/// What the window does next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Nothing.
    Nothing,
    /// Ask the plugin to draw the panel now.
    Render,
    /// Ask the plugin to draw the panel after this wait.
    RenderAfter(Duration),
    /// Send this event to the plugin now.
    Event(WidgetEvent),
}

/// What a finished call came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// What to do next.
    pub step: Step,
    /// The call failed: the window asks the manager to reload, so a fault shows up.
    pub failed: bool,
}

/// One panel's state.
#[derive(Debug)]
#[allow(clippy::struct_excessive_bools)] // four independent flags; a state machine would only rename them
pub struct PanelSlot {
    view: SlotView,
    in_flight: bool,
    refresh_queued: bool,
    events: VecDeque<WidgetEvent>,
    last_render: Option<Instant>,
    shown: bool,
    stale: bool,
}

impl Default for PanelSlot {
    fn default() -> Self {
        Self::new()
    }
}

impl PanelSlot {
    /// A slot that was never drawn and is hidden.
    #[must_use]
    pub fn new() -> Self {
        Self {
            view: SlotView::Loading,
            in_flight: false,
            refresh_queued: false,
            events: VecDeque::new(),
            last_render: None,
            shown: false,
            stale: true,
        }
    }

    /// What the slot shows.
    #[must_use]
    pub fn view(&self) -> &SlotView {
        &self.view
    }

    /// The tree on screen, if any.
    #[must_use]
    pub fn tree(&self) -> Option<&WidgetTree> {
        match &self.view {
            SlotView::Tree(t) => Some(t),
            _ => None,
        }
    }

    /// Whether a call is in flight.
    #[must_use]
    pub fn busy(&self) -> bool {
        self.in_flight
    }

    /// Whether a refresh arrived while the panel was hidden.
    #[must_use]
    pub fn stale(&self) -> bool {
        self.stale
    }

    /// How many events wait.
    #[must_use]
    pub fn queued_events(&self) -> usize {
        self.events.len()
    }

    /// The panel became shown or hidden. Showing a stale (or never drawn) panel renders it.
    pub fn set_shown(&mut self, shown: bool, now: Instant) -> Step {
        let was = self.shown;
        self.shown = shown;
        if shown && !was && self.stale {
            return self.begin_render(now);
        }
        Step::Nothing
    }

    /// The plugin asked for a redraw (or the user asked to reload).
    pub fn refresh(&mut self, now: Instant) -> Step {
        if !self.shown {
            self.stale = true;
            return Step::Nothing;
        }
        if self.in_flight {
            self.refresh_queued = true;
            return Step::Nothing;
        }
        self.begin_render(now)
    }

    /// The user touched a widget.
    pub fn event(&mut self, event: WidgetEvent, now: Instant) -> Step {
        let _ = now;
        if self.in_flight {
            if self.events.len() < EVENT_QUEUE {
                self.events.push_back(event);
            }
            return Step::Nothing;
        }
        self.in_flight = true;
        Step::Event(event)
    }

    /// A render finished.
    pub fn rendered(&mut self, result: PanelRender, now: Instant) -> Outcome {
        self.in_flight = false;
        let failed = result.is_err();
        self.view = match result {
            Ok(tree) => {
                self.stale = false;
                SlotView::Tree(tree)
            }
            Err(reason) => SlotView::Failed(reason),
        };
        Outcome {
            step: self.next(now),
            failed,
        }
    }

    /// An event call finished. A reply tree replaces the one on screen; no reply keeps it.
    pub fn event_replied(
        &mut self,
        result: Result<Option<WidgetTree>, String>,
        now: Instant,
    ) -> Outcome {
        self.in_flight = false;
        let failed = result.is_err();
        match result {
            Ok(Some(tree)) => self.view = SlotView::Tree(tree),
            Ok(None) => {}
            Err(reason) => self.view = SlotView::Failed(reason),
        }
        Outcome {
            step: self.next(now),
            failed,
        }
    }

    /// The panel is gone (its plugin was switched off): drop what waits.
    pub fn clear_queue(&mut self) {
        self.events.clear();
        self.refresh_queued = false;
    }

    fn begin_render(&mut self, now: Instant) -> Step {
        self.in_flight = true;
        let due = self.last_render.map(|at| at + RENDER_GAP);
        match due {
            Some(due) if due > now => {
                self.last_render = Some(due);
                Step::RenderAfter(due - now)
            }
            _ => {
                self.last_render = Some(now);
                Step::Render
            }
        }
    }

    /// After a call: queued events go first (they are the user's), then one queued refresh.
    fn next(&mut self, now: Instant) -> Step {
        if !self.shown {
            // Hidden meanwhile: nothing runs; the next show renders again.
            self.events.clear();
            if self.refresh_queued {
                self.stale = true;
            }
            self.refresh_queued = false;
            return Step::Nothing;
        }
        if let Some(event) = self.events.pop_front() {
            self.in_flight = true;
            return Step::Event(event);
        }
        if std::mem::take(&mut self.refresh_queued) {
            return self.begin_render(now);
        }
        Step::Nothing
    }
}

#[cfg(test)]
mod tests {
    use sqler_plugin_api::{Direction, Gap, Stack, Widget, WidgetNode};

    use super::*;

    fn tree(n: usize) -> WidgetTree {
        let kids: Vec<u32> = (1..=u32::try_from(n).unwrap()).collect();
        let mut nodes = vec![WidgetNode {
            widget: Widget::Stack(Stack {
                direction: Direction::Vertical,
                gap: Gap::Small,
            }),
            children: kids,
        }];
        nodes.extend((0..n).map(|_| WidgetNode {
            widget: Widget::Divider,
            children: Vec::new(),
        }));
        WidgetTree::new(nodes).unwrap()
    }

    fn click() -> WidgetEvent {
        WidgetEvent::Clicked("go".to_owned())
    }

    fn shown() -> (PanelSlot, Instant) {
        let t0 = Instant::now();
        let mut s = PanelSlot::new();
        assert_eq!(s.set_shown(true, t0), Step::Render);
        (s, t0)
    }

    #[test]
    fn a_new_slot_renders_when_first_shown() {
        let t0 = Instant::now();
        let mut s = PanelSlot::new();
        assert_eq!(*s.view(), SlotView::Loading);
        assert_eq!(s.set_shown(false, t0), Step::Nothing);
        assert_eq!(s.set_shown(true, t0), Step::Render);
        assert!(s.busy());
        let out = s.rendered(Ok(tree(1)), t0);
        assert_eq!(out.step, Step::Nothing);
        assert!(!out.failed);
        assert!(s.tree().is_some());
        assert!(!s.stale());
        // Hiding and showing again does not render a fresh panel.
        s.set_shown(false, t0);
        assert_eq!(s.set_shown(true, t0), Step::Nothing);
    }

    #[test]
    fn a_refresh_while_hidden_marks_it_stale_and_the_next_show_renders() {
        let t0 = Instant::now();
        let (mut s, _) = shown();
        s.rendered(Ok(tree(1)), t0);
        s.set_shown(false, t0);
        assert_eq!(s.refresh(t0), Step::Nothing);
        assert!(s.stale());
        let later = t0 + Duration::from_secs(1);
        assert_eq!(s.set_shown(true, later), Step::Render);
    }

    #[test]
    fn refreshes_during_a_call_coalesce_into_one_follow_up() {
        let (mut s, t0) = shown();
        for _ in 0..50 {
            assert_eq!(s.refresh(t0), Step::Nothing);
        }
        let t1 = t0 + Duration::from_millis(500);
        let out = s.rendered(Ok(tree(1)), t1);
        assert_eq!(out.step, Step::Render, "one follow-up");
        let out = s.rendered(Ok(tree(2)), t1);
        assert_eq!(out.step, Step::Nothing, "and only one");
    }

    #[test]
    fn renders_are_at_least_100_ms_apart() {
        let (mut s, t0) = shown();
        let t1 = t0 + Duration::from_millis(30);
        s.refresh(t1);
        let out = s.rendered(Ok(tree(1)), t1);
        assert_eq!(out.step, Step::RenderAfter(Duration::from_millis(70)));
        assert!(s.busy(), "the wait counts as the call in flight");
        // A refresh now only queues.
        assert_eq!(s.refresh(t1), Step::Nothing);
        let t2 = t0 + Duration::from_millis(100);
        let out = s.rendered(Ok(tree(1)), t2);
        assert_eq!(out.step, Step::RenderAfter(Duration::from_millis(100)));
        // Far enough apart: immediate.
        let mut s2 = PanelSlot::new();
        s2.set_shown(true, t0);
        s2.rendered(Ok(tree(1)), t0);
        assert_eq!(s2.refresh(t0 + RENDER_GAP), Step::Render);
    }

    #[test]
    fn events_wait_for_the_call_and_the_ninth_is_dropped() {
        let (mut s, t0) = shown();
        for _ in 0..12 {
            assert_eq!(s.event(click(), t0), Step::Nothing);
        }
        assert_eq!(s.queued_events(), EVENT_QUEUE);
        let mut sent = 0;
        let mut out = s.rendered(Ok(tree(1)), t0);
        while let Step::Event(_) = out.step {
            sent += 1;
            out = s.event_replied(Ok(None), t0);
        }
        assert_eq!(sent, EVENT_QUEUE);
        assert_eq!(out.step, Step::Nothing);
        assert!(!s.busy());
    }

    #[test]
    fn an_event_with_nothing_in_flight_goes_at_once() {
        let (mut s, t0) = shown();
        s.rendered(Ok(tree(1)), t0);
        assert_eq!(s.event(click(), t0), Step::Event(click()));
        assert!(s.busy());
        let out = s.event_replied(Ok(Some(tree(3))), t0);
        assert_eq!(out.step, Step::Nothing);
        assert_eq!(s.tree().unwrap().nodes().len(), 4);
        // No reply keeps the tree.
        s.event(click(), t0);
        s.event_replied(Ok(None), t0);
        assert_eq!(s.tree().unwrap().nodes().len(), 4);
    }

    #[test]
    fn a_failed_call_shows_the_reason_and_asks_for_a_reload() {
        let (mut s, t0) = shown();
        let out = s.rendered(Err("it broke".into()), t0);
        assert!(out.failed);
        assert_eq!(*s.view(), SlotView::Failed("it broke".into()));
        // A failed event keeps going with what waits.
        let mut s = PanelSlot::new();
        s.set_shown(true, t0);
        s.rendered(Ok(tree(1)), t0);
        s.event(click(), t0);
        s.event(click(), t0);
        let out = s.event_replied(Err("trap".into()), t0);
        assert!(out.failed);
        assert_eq!(out.step, Step::Event(click()));
    }

    #[test]
    fn hiding_during_a_call_runs_nothing_after_it() {
        let (mut s, t0) = shown();
        s.refresh(t0);
        s.event(click(), t0);
        s.set_shown(false, t0);
        let out = s.rendered(Ok(tree(1)), t0);
        assert_eq!(out.step, Step::Nothing);
        assert!(s.stale(), "the queued refresh is kept as stale");
        assert_eq!(s.queued_events(), 0);
    }
}
