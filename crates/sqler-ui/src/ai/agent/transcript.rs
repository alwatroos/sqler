// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! What an agent tab shows of the conversation (ADR-0051): messages, thoughts, tool calls, the
//! plan and the notices, in order. Plain Rust.
//!
//! The agent streams chunks, so consecutive chunks of one kind merge into one entry, a tool call
//! is updated in place by its id, and the plan of a turn is one entry that is replaced. The tab
//! keeps at most [`MAX_ENTRIES`] entries (the older ones are counted in `dropped` and the view
//! says so), each entry at most [`MAX_ENTRY_BYTES`], and the last [`STDERR_LINES`] lines of the
//! agent's stderr.

use std::collections::VecDeque;

use sqler_acp::{PlanEntry, SessionUpdate, ToolCallInfo, ToolCallStatus};

pub use crate::ai::session::{DROPPED_MARKER, MAX_ENTRIES};

/// The most bytes of text one entry keeps; the rest is cut and the entry says so.
pub const MAX_ENTRY_BYTES: usize = 64 * 1024;
/// The most steps of a plan an entry keeps.
pub const MAX_PLAN_STEPS: usize = 256;
/// The most lines of the agent's stderr that are kept.
pub const STDERR_LINES: usize = 64;
/// The longest stderr line that is kept, in bytes.
const MAX_STDERR_LINE: usize = 2048;
/// The title of a tool call that has none yet.
const UNTITLED_TOOL: &str = "Tool call";

/// Text that grows by chunks, up to [`MAX_ENTRY_BYTES`].
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Chunked {
    /// The text so far.
    pub text: String,
    /// Whether text was cut at the limit.
    pub cut: bool,
}

// Never the text.
impl std::fmt::Debug for Chunked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Chunked")
            .field("len", &self.text.len())
            .field("cut", &self.cut)
            .finish()
    }
}

impl Chunked {
    fn new(chunk: &str) -> Self {
        let mut this = Self::default();
        this.push(chunk);
        this
    }

    fn push(&mut self, chunk: &str) {
        if self.cut {
            return;
        }
        let room = MAX_ENTRY_BYTES - self.text.len();
        if chunk.len() <= room {
            self.text.push_str(chunk);
            return;
        }
        let mut end = room;
        while !chunk.is_char_boundary(end) {
            end -= 1;
        }
        self.text.push_str(&chunk[..end]);
        self.cut = true;
    }
}

/// One tool call of the agent.
#[derive(Clone, PartialEq, Eq)]
pub struct ToolEntry {
    /// The id the agent gave the call; stable across its updates.
    pub id: String,
    /// What the call does.
    pub title: String,
    /// `read`, `edit`, `execute`, ... when the agent said.
    pub kind: Option<String>,
    /// Where the call is.
    pub status: ToolCallStatus,
    /// Text the agent attached, capped as a whole.
    pub text: Vec<String>,
}

// Never the title or the text: they come from the agent.
impl std::fmt::Debug for ToolEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolEntry")
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

/// One line of the conversation.
#[derive(Clone, PartialEq, Eq)]
pub enum AgentEntry {
    /// What the user sent.
    User(Chunked),
    /// What the agent answered.
    Message(Chunked),
    /// What the agent reasoned.
    Thought(Chunked),
    /// A tool call.
    Tool(ToolEntry),
    /// The plan of the turn.
    Plan(Vec<PlanEntry>),
    /// A line from `SQLer`: a turn that failed or was cancelled.
    Notice(String),
}

// Never the text.
impl std::fmt::Debug for AgentEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::User(_) => "AgentEntry::User(..)",
            Self::Message(_) => "AgentEntry::Message(..)",
            Self::Thought(_) => "AgentEntry::Thought(..)",
            Self::Tool(_) => "AgentEntry::Tool(..)",
            Self::Plan(_) => "AgentEntry::Plan(..)",
            Self::Notice(_) => "AgentEntry::Notice(..)",
        })
    }
}

/// The conversation of one agent tab.
#[derive(Default)]
pub struct Transcript {
    entries: Vec<AgentEntry>,
    /// How many entries before `entries[0]` were dropped.
    dropped: usize,
    /// Where the plan of the current turn is.
    plan: Option<usize>,
    stderr: VecDeque<String>,
}

// Never the entries.
impl std::fmt::Debug for Transcript {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Transcript")
            .field("entries", &self.entries.len())
            .field("dropped", &self.dropped)
            .field("stderr", &self.stderr.len())
            .finish_non_exhaustive()
    }
}

impl Transcript {
    /// The entries that are kept, oldest first.
    #[must_use]
    pub fn entries(&self) -> &[AgentEntry] {
        &self.entries
    }

    /// How many older entries were dropped; the view shows [`DROPPED_MARKER`] when it is not 0.
    #[must_use]
    pub fn dropped(&self) -> usize {
        self.dropped
    }

    /// The last lines the agent wrote to stderr, oldest first.
    #[must_use]
    pub fn stderr(&self) -> Vec<String> {
        self.stderr.iter().cloned().collect()
    }

    /// A new turn starts: its plan is a new entry.
    pub fn begin_turn(&mut self) {
        self.plan = None;
    }

    /// Adds what the user sent.
    pub fn push_user(&mut self, text: &str) {
        self.push(AgentEntry::User(Chunked::new(text)));
    }

    /// Adds a line from `SQLer`.
    pub fn push_notice(&mut self, text: impl Into<String>) {
        let text: String = text.into();
        self.push(AgentEntry::Notice(
            cut_str(&text, MAX_ENTRY_BYTES).to_owned(),
        ));
    }

    /// Keeps a line of the agent's stderr.
    pub fn push_stderr(&mut self, line: &str) {
        let mut end = line.len().min(MAX_STDERR_LINE);
        while !line.is_char_boundary(end) {
            end -= 1;
        }
        if self.stderr.len() == STDERR_LINES {
            self.stderr.pop_front();
        }
        self.stderr.push_back(line[..end].to_owned());
    }

    /// Applies one streamed update of the agent.
    pub fn apply(&mut self, update: &SessionUpdate) {
        match update {
            SessionUpdate::AgentMessage(chunk) => match self.entries.last_mut() {
                Some(AgentEntry::Message(text)) => text.push(chunk),
                _ => self.push(AgentEntry::Message(Chunked::new(chunk))),
            },
            SessionUpdate::AgentThought(chunk) => match self.entries.last_mut() {
                Some(AgentEntry::Thought(text)) => text.push(chunk),
                _ => self.push(AgentEntry::Thought(Chunked::new(chunk))),
            },
            SessionUpdate::ToolCall(info) => self.apply_tool(info),
            SessionUpdate::Plan(steps) => self.apply_plan(steps),
            // The user's own text is already there; the rest is not rendered.
            _ => {}
        }
    }

    fn apply_tool(&mut self, info: &ToolCallInfo) {
        let found = self
            .entries
            .iter()
            .rposition(|e| matches!(e, AgentEntry::Tool(t) if t.id == info.id));
        let Some(AgentEntry::Tool(tool)) = found.and_then(|ix| self.entries.get_mut(ix)) else {
            let mut tool = ToolEntry {
                id: info.id.clone(),
                title: UNTITLED_TOOL.to_owned(),
                kind: None,
                status: ToolCallStatus::Pending,
                text: Vec::new(),
            };
            merge_tool(&mut tool, info);
            self.push(AgentEntry::Tool(tool));
            return;
        };
        merge_tool(tool, info);
    }

    fn apply_plan(&mut self, steps: &[PlanEntry]) {
        let steps = capped_plan(steps);
        if let Some(AgentEntry::Plan(plan)) = self.plan.and_then(|ix| self.entries.get_mut(ix)) {
            *plan = steps;
            return;
        }
        self.push(AgentEntry::Plan(steps));
        self.plan = Some(self.entries.len() - 1);
    }

    fn push(&mut self, entry: AgentEntry) {
        self.entries.push(entry);
        let extra = self.entries.len().saturating_sub(MAX_ENTRIES);
        if extra > 0 {
            self.entries.drain(..extra);
            self.dropped += extra;
            self.plan = self.plan.and_then(|ix| ix.checked_sub(extra));
        }
    }
}

/// The longest prefix of `text` that is at most `limit` bytes and ends on a character boundary.
pub(super) fn cut_str(text: &str, limit: usize) -> &str {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// The plan of the agent as an entry: at most [`MAX_PLAN_STEPS`] steps and [`MAX_ENTRY_BYTES`] bytes
/// of text in all.
fn capped_plan(steps: &[PlanEntry]) -> Vec<PlanEntry> {
    let mut room = MAX_ENTRY_BYTES;
    let mut kept = Vec::new();
    for step in steps.iter().take(MAX_PLAN_STEPS) {
        if room == 0 {
            break;
        }
        let mut take = |text: &str| {
            let part = cut_str(text, room);
            room -= part.len();
            part.to_owned()
        };
        kept.push(PlanEntry {
            content: take(&step.content),
            priority: take(&step.priority),
            status: take(&step.status),
        });
    }
    kept
}

/// Puts the fields the agent sent into `tool`; absent fields stay as they were.
fn merge_tool(tool: &mut ToolEntry, info: &ToolCallInfo) {
    if let Some(title) = &info.title {
        cut_str(title, MAX_ENTRY_BYTES).clone_into(&mut tool.title);
    }
    if let Some(kind) = &info.kind {
        tool.kind = Some(cut_str(kind, MAX_ENTRY_BYTES).to_owned());
    }
    if let Some(status) = info.status {
        tool.status = status;
    }
    if !info.text.is_empty() {
        let mut room = MAX_ENTRY_BYTES;
        tool.text = Vec::new();
        for line in &info.text {
            if room == 0 {
                break;
            }
            let part = cut_str(line, room);
            tool.text.push(part.to_owned());
            room -= part.len();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(text: &str) -> SessionUpdate {
        SessionUpdate::AgentMessage(text.to_owned())
    }

    fn tool(id: &str, title: Option<&str>, status: Option<ToolCallStatus>) -> SessionUpdate {
        SessionUpdate::ToolCall(ToolCallInfo {
            id: id.to_owned(),
            title: title.map(str::to_owned),
            kind: None,
            status,
            text: Vec::new(),
        })
    }

    fn plan(steps: &[&str]) -> SessionUpdate {
        SessionUpdate::Plan(
            steps
                .iter()
                .map(|s| PlanEntry {
                    content: (*s).to_owned(),
                    priority: "medium".to_owned(),
                    status: "pending".to_owned(),
                })
                .collect(),
        )
    }

    #[test]
    fn consecutive_chunks_of_one_kind_merge() {
        let mut t = Transcript::default();
        t.apply(&message("Hello"));
        t.apply(&message(" world"));
        t.apply(&SessionUpdate::AgentThought("hm".into()));
        t.apply(&SessionUpdate::AgentThought("m".into()));
        t.apply(&message("again"));
        let kinds: Vec<_> = t.entries().iter().map(|e| format!("{e:?}")).collect();
        assert_eq!(
            kinds,
            [
                "AgentEntry::Message(..)",
                "AgentEntry::Thought(..)",
                "AgentEntry::Message(..)"
            ]
        );
        let AgentEntry::Message(first) = &t.entries()[0] else {
            panic!("not a message");
        };
        assert_eq!(first.text, "Hello world");
        let AgentEntry::Thought(thought) = &t.entries()[1] else {
            panic!("not a thought");
        };
        assert_eq!(thought.text, "hmm");
    }

    #[test]
    fn a_tool_call_is_updated_in_place_by_its_id() {
        let mut t = Transcript::default();
        t.apply(&tool(
            "a",
            Some("List tables"),
            Some(ToolCallStatus::InProgress),
        ));
        t.apply(&message("between"));
        t.apply(&tool("b", Some("Other"), None));
        // An update with only a status keeps the title.
        t.apply(&tool("a", None, Some(ToolCallStatus::Completed)));
        assert_eq!(t.entries().len(), 3);
        let AgentEntry::Tool(a) = &t.entries()[0] else {
            panic!("not a tool");
        };
        assert_eq!(
            (a.title.as_str(), a.status),
            ("List tables", ToolCallStatus::Completed)
        );
        let AgentEntry::Tool(b) = &t.entries()[2] else {
            panic!("not a tool");
        };
        assert_eq!(
            (b.title.as_str(), b.status),
            ("Other", ToolCallStatus::Pending)
        );
        // A call that starts with no title gets a placeholder.
        t.apply(&tool("c", None, None));
        let AgentEntry::Tool(c) = &t.entries()[3] else {
            panic!("not a tool");
        };
        assert_eq!(c.title, UNTITLED_TOOL);
    }

    #[test]
    fn the_plan_is_one_entry_per_turn_and_replaced() {
        let mut t = Transcript::default();
        t.begin_turn();
        t.apply(&plan(&["one"]));
        t.apply(&message("x"));
        t.apply(&plan(&["one", "two"]));
        let plans: Vec<_> = t
            .entries()
            .iter()
            .filter_map(|e| match e {
                AgentEntry::Plan(p) => Some(p.len()),
                _ => None,
            })
            .collect();
        assert_eq!(plans, [2]);
        t.begin_turn();
        t.apply(&plan(&["next"]));
        let plans = t
            .entries()
            .iter()
            .filter(|e| matches!(e, AgentEntry::Plan(_)))
            .count();
        assert_eq!(plans, 2);
    }

    #[test]
    fn the_transcript_keeps_the_newest_entries_and_counts_the_rest() {
        let mut t = Transcript::default();
        t.begin_turn();
        t.apply(&plan(&["early"]));
        for ix in 0..MAX_ENTRIES + 100 {
            t.push_notice(format!("n{ix}"));
        }
        assert_eq!(t.entries().len(), MAX_ENTRIES);
        assert_eq!(t.dropped(), 101);
        assert!(matches!(&t.entries()[0], AgentEntry::Notice(n) if n == "n100"));
        // The plan that was dropped is not updated in a wrong place: a new one is added.
        t.apply(&plan(&["late"]));
        let plans = t
            .entries()
            .iter()
            .filter(|e| matches!(e, AgentEntry::Plan(_)))
            .count();
        assert_eq!(plans, 1);
        assert_eq!(t.entries().len(), MAX_ENTRIES);
    }

    #[test]
    fn an_entry_is_cut_at_the_byte_limit() {
        let mut t = Transcript::default();
        let chunk = "é".repeat(10_000);
        for _ in 0..10 {
            t.apply(&message(&chunk));
        }
        let AgentEntry::Message(m) = &t.entries()[0] else {
            panic!("not a message");
        };
        assert!(m.cut);
        assert!(m.text.len() <= MAX_ENTRY_BYTES);
        assert!(m.text.len() > MAX_ENTRY_BYTES - 4);
        t.push_user(&"u".repeat(MAX_ENTRY_BYTES * 2));
        let AgentEntry::User(u) = t.entries().last().unwrap() else {
            panic!("not a user entry");
        };
        assert_eq!(u.text.len(), MAX_ENTRY_BYTES);
        assert!(u.cut);
        // The text of a tool call is capped as a whole.
        t.apply(&SessionUpdate::ToolCall(ToolCallInfo {
            id: "t".into(),
            title: Some("big".into()),
            kind: None,
            status: None,
            text: vec!["x".repeat(MAX_ENTRY_BYTES), "y".repeat(MAX_ENTRY_BYTES)],
        }));
        let AgentEntry::Tool(tool) = t.entries().last().unwrap() else {
            panic!("not a tool");
        };
        assert_eq!(
            tool.text.iter().map(String::len).sum::<usize>(),
            MAX_ENTRY_BYTES
        );
    }

    #[test]
    fn plan_steps_tool_titles_and_notices_are_capped() {
        let mut t = Transcript::default();
        let step = |n: usize| PlanEntry {
            content: "p".repeat(n),
            priority: "high".into(),
            status: "pending".into(),
        };
        t.apply(&SessionUpdate::Plan(vec![
            step(MAX_ENTRY_BYTES * 2),
            step(10),
        ]));
        let AgentEntry::Plan(plan) = t.entries().last().unwrap() else {
            panic!("not a plan");
        };
        let bytes: usize = plan
            .iter()
            .map(|p| p.content.len() + p.priority.len() + p.status.len())
            .sum();
        assert!(bytes <= MAX_ENTRY_BYTES, "{bytes}");
        let many = vec![step(1); MAX_PLAN_STEPS * 4];
        t.apply(&SessionUpdate::Plan(many));
        let AgentEntry::Plan(plan) = t.entries().last().unwrap() else {
            panic!("not a plan");
        };
        assert_eq!(plan.len(), MAX_PLAN_STEPS);
        t.apply(&SessionUpdate::ToolCall(ToolCallInfo {
            id: "t".into(),
            title: Some("é".repeat(MAX_ENTRY_BYTES)),
            kind: Some("k".repeat(MAX_ENTRY_BYTES * 2)),
            status: None,
            text: Vec::new(),
        }));
        let AgentEntry::Tool(tool) = t.entries().last().unwrap() else {
            panic!("not a tool");
        };
        assert!(tool.title.len() <= MAX_ENTRY_BYTES);
        assert_eq!(tool.kind.as_ref().map(String::len), Some(MAX_ENTRY_BYTES));
        t.push_notice("n".repeat(MAX_ENTRY_BYTES * 2));
        let AgentEntry::Notice(text) = t.entries().last().unwrap() else {
            panic!("not a notice");
        };
        assert_eq!(text.len(), MAX_ENTRY_BYTES);
    }

    #[test]
    fn stderr_is_a_ring_of_bounded_lines() {
        let mut t = Transcript::default();
        for ix in 0..STDERR_LINES + 10 {
            t.push_stderr(&format!("line {ix}"));
        }
        let lines = t.stderr();
        assert_eq!(lines.len(), STDERR_LINES);
        assert_eq!(lines[0], "line 10");
        t.push_stderr(&"é".repeat(MAX_STDERR_LINE));
        assert!(t.stderr().last().unwrap().len() <= MAX_STDERR_LINE);
    }

    #[test]
    fn debug_never_shows_the_conversation() {
        let mut t = Transcript::default();
        t.push_user("canary-user");
        t.apply(&message("canary-message"));
        t.apply(&tool("c", Some("canary-title"), None));
        t.push_stderr("canary-stderr");
        let text = format!("{t:?} {:?}", t.entries());
        assert!(!text.contains("canary"));
    }
}
