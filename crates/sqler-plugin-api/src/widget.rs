// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The declarative widget tree and its validation (mirrors `widgets.wit`).
//!
//! Only semantic styles and gap tokens exist: no colours, sizes or pixels.

use std::collections::HashSet;

use sqler_core::{DataType, ObjectKind};
use thiserror::Error;

use crate::id::ContributionId;
use crate::limits::Limits;

/// The case names of the WIT `widget` variant, in order (checked against `widgets.wit` by the drift test).
pub const WIDGET_CASES: &[&str] = &[
    "stack",
    "text",
    "button",
    "text-input",
    "checkbox",
    "select",
    "table",
    "divider",
];

/// Stack direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Top to bottom.
    Vertical,
    /// Left to right.
    Horizontal,
}

/// Spacing token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gap {
    /// No gap.
    None,
    /// Small gap.
    Small,
    /// Medium gap.
    Medium,
    /// Large gap.
    Large,
}

/// Semantic text style.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextStyle {
    /// Normal text.
    Body,
    /// De-emphasised text.
    Muted,
    /// A heading.
    Heading,
    /// Monospace code.
    Code,
}

/// Semantic button style.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonStyle {
    /// The main action.
    Primary,
    /// A secondary action.
    Secondary,
}

/// A container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stack {
    /// Direction.
    pub direction: Direction,
    /// Gap between children.
    pub gap: Gap,
}

/// A text label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Text {
    /// Content.
    pub content: String,
    /// Style.
    pub style: TextStyle,
}

/// A button.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Button {
    /// Element id.
    pub id: String,
    /// Label.
    pub label: String,
    /// Style.
    pub style: ButtonStyle,
}

/// A single-line input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextInput {
    /// Element id.
    pub id: String,
    /// Current value.
    pub value: String,
    /// Placeholder.
    pub placeholder: Option<String>,
}

/// A checkbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkbox {
    /// Element id.
    pub id: String,
    /// Label.
    pub label: String,
    /// Checked state.
    pub checked: bool,
}

/// A drop-down.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Select {
    /// Element id.
    pub id: String,
    /// Options.
    pub options: Vec<String>,
    /// Selected index.
    pub selected: Option<u32>,
}

/// A read-only table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    /// Column headings.
    pub columns: Vec<String>,
    /// Rows of cells.
    pub rows: Vec<Vec<String>>,
}

/// One widget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Widget {
    /// A container.
    Stack(Stack),
    /// A text label.
    Text(Text),
    /// A button.
    Button(Button),
    /// A text input.
    TextInput(TextInput),
    /// A checkbox.
    Checkbox(Checkbox),
    /// A drop-down.
    Select(Select),
    /// A table.
    Table(Table),
    /// A divider line.
    Divider,
}

/// A widget and the indices of its children (only a stack has children).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WidgetNode {
    /// The widget.
    pub widget: Widget,
    /// Child node indices.
    pub children: Vec<u32>,
}

/// Why a widget tree was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum WidgetError {
    /// No nodes.
    #[error("widget tree is empty")]
    Empty,
    /// More nodes than the limit.
    #[error("widget tree has too many nodes")]
    TooManyNodes,
    /// Nested deeper than the limit.
    #[error("widget tree is nested too deeply")]
    TooDeep,
    /// A child index is outside the node list.
    #[error("child index out of range")]
    ChildOutOfRange,
    /// A node has two parents, the root has a parent, or a node is unreachable (a cycle).
    #[error("nodes do not form a tree rooted at node 0")]
    NotATree,
    /// A node that is not a stack has children.
    #[error("only a stack may have children")]
    ChildrenOnLeaf,
    /// Two elements share an id.
    #[error("duplicate element id")]
    DuplicateElementId,
    /// An element id is not a valid id.
    #[error("bad element id")]
    BadElementId,
    /// A string is longer than the limit, or the text of the whole tree is over the total budget.
    #[error("text is too long")]
    TextTooLong,
    /// A table has too many rows or columns, a row has the wrong width, or a select has too many options.
    #[error("table or select is too large")]
    TableTooLarge,
    /// A selected index is outside the options.
    #[error("selected index out of range")]
    BadSelection,
}

/// A validated widget tree. Node 0 is the root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WidgetTree {
    nodes: Vec<WidgetNode>,
}

impl WidgetTree {
    /// Validates the nodes against `Limits::DEFAULT.widgets`.
    pub fn new(nodes: Vec<WidgetNode>) -> Result<Self, WidgetError> {
        let limits = Limits::DEFAULT.widgets;
        if nodes.is_empty() {
            return Err(WidgetError::Empty);
        }
        if nodes.len() > limits.max_nodes {
            return Err(WidgetError::TooManyNodes);
        }
        let n = nodes.len();
        let mut parents = vec![0u8; n];
        let mut ids: HashSet<&str> = HashSet::new();
        let mut budget = limits.max_total_text_bytes;
        for node in &nodes {
            if !node.children.is_empty() && !matches!(node.widget, Widget::Stack(_)) {
                return Err(WidgetError::ChildrenOnLeaf);
            }
            for &child in &node.children {
                let child = child as usize;
                if child >= n {
                    return Err(WidgetError::ChildOutOfRange);
                }
                if child == 0 || parents[child] > 0 {
                    return Err(WidgetError::NotATree);
                }
                parents[child] += 1;
            }
            check_widget(&node.widget, &limits, &mut ids, &mut budget)?;
        }
        // Depth-first from the root; each node has at most one parent, so this terminates.
        let mut seen = 0usize;
        let mut stack: Vec<(usize, usize)> = vec![(0, 1)];
        while let Some((index, depth)) = stack.pop() {
            if depth > limits.max_depth {
                return Err(WidgetError::TooDeep);
            }
            seen += 1;
            for &child in &nodes[index].children {
                stack.push((child as usize, depth + 1));
            }
        }
        if seen != n {
            return Err(WidgetError::NotATree);
        }
        Ok(Self { nodes })
    }

    /// The nodes; the root is node 0.
    #[must_use]
    pub fn nodes(&self) -> &[WidgetNode] {
        &self.nodes
    }

    /// The root node.
    #[must_use]
    pub fn root(&self) -> &WidgetNode {
        &self.nodes[0]
    }
}

/// Checks one string against the per-string limit and takes it off the remaining whole-tree text budget.
fn check_str(s: &str, max: usize, budget: &mut usize) -> Result<(), WidgetError> {
    if s.len() > max || s.len() > *budget {
        return Err(WidgetError::TextTooLong);
    }
    *budget -= s.len();
    Ok(())
}

fn check_id<'a>(id: &'a str, ids: &mut HashSet<&'a str>) -> Result<(), WidgetError> {
    id.parse::<ContributionId>()
        .map_err(|_| WidgetError::BadElementId)?;
    if ids.insert(id) {
        Ok(())
    } else {
        Err(WidgetError::DuplicateElementId)
    }
}

fn check_widget<'a>(
    widget: &'a Widget,
    limits: &crate::limits::WidgetLimits,
    ids: &mut HashSet<&'a str>,
    budget: &mut usize,
) -> Result<(), WidgetError> {
    let max = limits.max_string_bytes;
    match widget {
        Widget::Stack(_) | Widget::Divider => {}
        Widget::Text(t) => check_str(&t.content, max, budget)?,
        Widget::Button(b) => {
            check_id(&b.id, ids)?;
            check_str(&b.label, max, budget)?;
        }
        Widget::TextInput(t) => {
            check_id(&t.id, ids)?;
            check_str(&t.value, max, budget)?;
            if let Some(p) = &t.placeholder {
                check_str(p, max, budget)?;
            }
        }
        Widget::Checkbox(c) => {
            check_id(&c.id, ids)?;
            check_str(&c.label, max, budget)?;
        }
        Widget::Select(s) => {
            check_id(&s.id, ids)?;
            if s.options.len() > limits.max_select_options {
                return Err(WidgetError::TableTooLarge);
            }
            for o in &s.options {
                check_str(o, max, budget)?;
            }
            if let Some(i) = s.selected
                && i as usize >= s.options.len()
            {
                return Err(WidgetError::BadSelection);
            }
        }
        Widget::Table(t) => {
            if t.columns.len() > limits.max_table_columns || t.rows.len() > limits.max_table_rows {
                return Err(WidgetError::TableTooLarge);
            }
            for c in &t.columns {
                check_str(c, max, budget)?;
            }
            for row in &t.rows {
                if row.len() != t.columns.len() {
                    return Err(WidgetError::TableTooLarge);
                }
                for cell in row {
                    check_str(cell, max, budget)?;
                }
            }
        }
    }
    Ok(())
}

/// A user interaction with a panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WidgetEvent {
    /// A button was clicked (its id).
    Clicked(String),
    /// A text input changed.
    Changed {
        /// Element id.
        id: String,
        /// New value.
        value: String,
    },
    /// A checkbox toggled.
    Toggled {
        /// Element id.
        id: String,
        /// New state.
        checked: bool,
    },
    /// A drop-down selection.
    Selected {
        /// Element id.
        id: String,
        /// Selected index.
        index: u32,
    },
}

/// A column as the plugin sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnRef {
    /// Name.
    pub name: String,
    /// Portable type.
    pub data_type: DataType,
    /// The engine's type name.
    pub type_name: String,
}

/// The path part of an object reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectPathRef {
    /// Database.
    pub database: String,
    /// Schema.
    pub schema: String,
    /// Table, for indexes and triggers.
    pub table: Option<String>,
}

/// A catalog object as the plugin sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectRef {
    /// Kind.
    pub kind: ObjectKind,
    /// Parent path.
    pub parent: ObjectPathRef,
    /// Name.
    pub name: String,
}

/// Where a command was invoked.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum CommandContext {
    /// No context (the palette).
    None,
    /// The editor.
    Editor {
        /// Connection id, if any.
        connection: Option<String>,
        /// Selected text.
        selection: String,
    },
    /// The object browser.
    Object {
        /// Connection id.
        connection: String,
        /// The object.
        object: ObjectRef,
    },
    /// The results grid.
    Results {
        /// Connection id.
        connection: String,
        /// The columns.
        columns: Vec<ColumnRef>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stack(children: &[u32]) -> WidgetNode {
        WidgetNode {
            widget: Widget::Stack(Stack {
                direction: Direction::Vertical,
                gap: Gap::Small,
            }),
            children: children.to_vec(),
        }
    }

    fn leaf() -> WidgetNode {
        WidgetNode {
            widget: Widget::Divider,
            children: vec![],
        }
    }

    fn button(id: &str) -> WidgetNode {
        WidgetNode {
            widget: Widget::Button(Button {
                id: id.into(),
                label: "ok".into(),
                style: ButtonStyle::Primary,
            }),
            children: vec![],
        }
    }

    #[test]
    fn valid_tree() {
        let t = WidgetTree::new(vec![stack(&[1, 2]), button("a"), button("b")]).unwrap();
        assert_eq!(t.nodes().len(), 3);
        assert!(matches!(t.root().widget, Widget::Stack(_)));
    }

    #[test]
    fn structure_errors() {
        use WidgetError as E;
        assert_eq!(WidgetTree::new(vec![]), Err(E::Empty));
        assert_eq!(WidgetTree::new(vec![stack(&[5])]), Err(E::ChildOutOfRange));
        assert_eq!(WidgetTree::new(vec![stack(&[0])]), Err(E::NotATree));
        // cycle: 1 -> 2 -> 1, unreachable from the root
        assert_eq!(
            WidgetTree::new(vec![stack(&[]), stack(&[2]), stack(&[1])]),
            Err(E::NotATree)
        );
        // two parents
        assert_eq!(
            WidgetTree::new(vec![stack(&[1, 2]), stack(&[2]), leaf()]),
            Err(E::NotATree)
        );
        // unreachable node
        assert_eq!(WidgetTree::new(vec![leaf(), leaf()]), Err(E::NotATree));
        // children on a leaf
        let bad = WidgetNode {
            widget: Widget::Divider,
            children: vec![1],
        };
        assert_eq!(WidgetTree::new(vec![bad, leaf()]), Err(E::ChildrenOnLeaf));
        // same child twice
        assert_eq!(
            WidgetTree::new(vec![stack(&[1, 1]), leaf()]),
            Err(E::NotATree)
        );
    }

    #[test]
    fn size_limits() {
        use WidgetError as E;
        let mut nodes = vec![stack(&(1..=2000).collect::<Vec<u32>>())];
        nodes.extend((0..2000).map(|_| leaf()));
        assert_eq!(WidgetTree::new(nodes), Err(E::TooManyNodes));
        let mut nodes = vec![stack(&(1..=1999).collect::<Vec<u32>>())];
        nodes.extend((0..1999).map(|_| leaf()));
        assert!(WidgetTree::new(nodes).is_ok());
        // depth 32 ok, 33 not
        for (depth, ok) in [(32usize, true), (33, false)] {
            let mut nodes: Vec<WidgetNode> = (0..depth - 1)
                .map(|i| stack(&[u32::try_from(i + 1).unwrap()]))
                .collect();
            nodes.push(leaf());
            assert_eq!(WidgetTree::new(nodes).is_ok(), ok, "depth {depth}");
        }
        // a very deep chain does not overflow the stack
        let mut nodes: Vec<WidgetNode> = (0..1998).map(|i| stack(&[i + 1])).collect();
        nodes.push(leaf());
        assert_eq!(WidgetTree::new(nodes), Err(E::TooDeep));
    }

    #[test]
    fn content_limits() {
        use WidgetError as E;
        let text = |n: usize| WidgetNode {
            widget: Widget::Text(Text {
                content: "a".repeat(n),
                style: TextStyle::Body,
            }),
            children: vec![],
        };
        assert!(WidgetTree::new(vec![text(16 * 1024)]).is_ok());
        assert_eq!(
            WidgetTree::new(vec![text(16 * 1024 + 1)]),
            Err(E::TextTooLong)
        );
        let table = |cols: usize, rows: usize| WidgetNode {
            widget: Widget::Table(Table {
                columns: vec!["c".into(); cols],
                rows: vec![vec!["x".into(); cols]; rows],
            }),
            children: vec![],
        };
        assert!(WidgetTree::new(vec![table(50, 1000)]).is_ok());
        // the total text of the tree is capped as well: 64 texts of 16 KiB fill the 1 MiB budget exactly
        let total = |n: usize| {
            let mut nodes = vec![stack(
                &(1..=u32::try_from(n).unwrap()).collect::<Vec<u32>>(),
            )];
            nodes.extend((0..n).map(|_| text(16 * 1024)));
            WidgetTree::new(nodes)
        };
        assert!(total(64).is_ok());
        assert_eq!(total(65), Err(E::TextTooLong));
        // many small cells add up too
        let big_table = WidgetNode {
            widget: Widget::Table(Table {
                columns: vec!["c".into(); 50],
                rows: vec![vec!["x".repeat(1024); 50]; 1000],
            }),
            children: vec![],
        };
        assert_eq!(WidgetTree::new(vec![big_table]), Err(E::TextTooLong));
        assert_eq!(WidgetTree::new(vec![table(51, 1)]), Err(E::TableTooLarge));
        assert_eq!(WidgetTree::new(vec![table(1, 1001)]), Err(E::TableTooLarge));
        let ragged = |cols: usize, row: usize| WidgetNode {
            widget: Widget::Table(Table {
                columns: vec!["c".into(); cols],
                rows: vec![vec!["x".into(); row]],
            }),
            children: vec![],
        };
        assert!(WidgetTree::new(vec![ragged(3, 3)]).is_ok());
        assert_eq!(WidgetTree::new(vec![ragged(3, 2)]), Err(E::TableTooLarge));
        assert_eq!(WidgetTree::new(vec![ragged(3, 4)]), Err(E::TableTooLarge));
        let select = |n: usize, sel: Option<u32>| WidgetNode {
            widget: Widget::Select(Select {
                id: "s".into(),
                options: vec!["o".into(); n],
                selected: sel,
            }),
            children: vec![],
        };
        assert!(WidgetTree::new(vec![select(64, Some(63))]).is_ok());
        assert_eq!(
            WidgetTree::new(vec![select(65, None)]),
            Err(E::TableTooLarge)
        );
        assert_eq!(
            WidgetTree::new(vec![select(2, Some(2))]),
            Err(E::BadSelection)
        );
    }

    #[test]
    fn element_ids() {
        use WidgetError as E;
        assert_eq!(
            WidgetTree::new(vec![stack(&[1, 2]), button("a"), button("a")]),
            Err(E::DuplicateElementId)
        );
        assert_eq!(WidgetTree::new(vec![button("A b")]), Err(E::BadElementId));
        assert_eq!(WidgetTree::new(vec![button("")]), Err(E::BadElementId));
    }
}
