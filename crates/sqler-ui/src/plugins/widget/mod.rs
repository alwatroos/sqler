// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The widget tree of a plugin panel on the window side (ADR-0060): the diff between two trees, which tells the view
//! which input editors to create, drop or update, and (behind the `gui` feature) the render.

use std::collections::BTreeMap;

use sqler_plugin_api::{Widget, WidgetTree};

#[cfg(feature = "gui")]
pub(crate) mod view;

/// What changed between two trees, for the elements that hold state in the window: text inputs and selects.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TreeDiff {
    /// The two trees are equal.
    pub same: bool,
    /// Ids that are new.
    pub inputs_added: Vec<String>,
    /// Ids that are gone.
    pub inputs_removed: Vec<String>,
    /// Ids in both whose value (the text, or the selected index) differs.
    pub inputs_value_changed: Vec<String>,
}

/// The id and value of every text input and select of a tree, in id order.
fn inputs(tree: &WidgetTree) -> BTreeMap<&str, String> {
    let mut out = BTreeMap::new();
    for node in tree.nodes() {
        match &node.widget {
            Widget::TextInput(t) => {
                out.insert(t.id.as_str(), t.value.clone());
            }
            Widget::Select(s) => {
                let value = s.selected.map_or_else(String::new, |i| i.to_string());
                out.insert(s.id.as_str(), value);
            }
            _ => {}
        }
    }
    out
}

/// Compares the tree on screen (`None` before the first one) with the new tree.
#[must_use]
pub fn diff(old: Option<&WidgetTree>, new: &WidgetTree) -> TreeDiff {
    let before = old.map(inputs).unwrap_or_default();
    let after = inputs(new);
    let mut d = TreeDiff {
        same: old.is_some_and(|o| o == new),
        ..TreeDiff::default()
    };
    for (id, value) in &after {
        match before.get(id) {
            None => d.inputs_added.push((*id).to_owned()),
            Some(v) if v != value => d.inputs_value_changed.push((*id).to_owned()),
            Some(_) => {}
        }
    }
    for id in before.keys() {
        if !after.contains_key(id) {
            d.inputs_removed.push((*id).to_owned());
        }
    }
    d
}

#[cfg(test)]
mod tests {
    use sqler_plugin_api::{
        Button, ButtonStyle, Direction, Gap, Select, Stack, TextInput, WidgetNode,
    };

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

    fn input(id: &str, value: &str) -> WidgetNode {
        WidgetNode {
            widget: Widget::TextInput(TextInput {
                id: id.into(),
                value: value.into(),
                placeholder: None,
            }),
            children: Vec::new(),
        }
    }

    fn select(id: &str, selected: Option<u32>) -> WidgetNode {
        WidgetNode {
            widget: Widget::Select(Select {
                id: id.into(),
                options: vec!["a".into(), "b".into()],
                selected,
            }),
            children: Vec::new(),
        }
    }

    fn button(id: &str) -> WidgetNode {
        WidgetNode {
            widget: Widget::Button(Button {
                id: id.into(),
                label: "go".into(),
                style: ButtonStyle::Primary,
            }),
            children: Vec::new(),
        }
    }

    fn tree(nodes: Vec<WidgetNode>) -> WidgetTree {
        WidgetTree::new(nodes).unwrap()
    }

    #[test]
    fn the_first_tree_adds_every_input() {
        let t = tree(vec![
            stack(&[1, 2, 3]),
            input("name", "x"),
            select("kind", Some(1)),
            button("go"),
        ]);
        let d = diff(None, &t);
        assert!(!d.same);
        assert_eq!(d.inputs_added, ["kind", "name"]);
        assert_eq!(d.inputs_removed.len(), 0);
        assert_eq!(d.inputs_value_changed.len(), 0);
    }

    #[test]
    fn equal_trees_are_the_same() {
        let t = tree(vec![stack(&[1]), input("name", "x")]);
        let d = diff(Some(&t), &t.clone());
        assert!(d.same);
        assert_eq!(
            d,
            TreeDiff {
                same: true,
                ..TreeDiff::default()
            }
        );
    }

    #[test]
    fn added_removed_and_changed_inputs() {
        let old = tree(vec![stack(&[1, 2]), input("name", "x"), input("gone", "")]);
        let new = tree(vec![stack(&[1, 2]), input("name", "y"), input("fresh", "")]);
        let d = diff(Some(&old), &new);
        assert!(!d.same);
        assert_eq!(d.inputs_added, ["fresh"]);
        assert_eq!(d.inputs_removed, ["gone"]);
        assert_eq!(d.inputs_value_changed, ["name"]);
    }

    #[test]
    fn a_select_change_is_a_value_change() {
        let old = tree(vec![stack(&[1]), select("kind", Some(0))]);
        let new = tree(vec![stack(&[1]), select("kind", Some(1))]);
        assert_eq!(diff(Some(&old), &new).inputs_value_changed, ["kind"]);
        let none = tree(vec![stack(&[1]), select("kind", None)]);
        assert_eq!(diff(Some(&new), &none).inputs_value_changed, ["kind"]);
    }

    #[test]
    fn buttons_and_text_do_not_count() {
        let old = tree(vec![stack(&[1]), button("a")]);
        let new = tree(vec![stack(&[1]), button("b")]);
        let d = diff(Some(&old), &new);
        assert!(!d.same);
        assert!(d.inputs_added.is_empty() && d.inputs_removed.is_empty());
        assert_eq!(d.inputs_value_changed.len(), 0);
    }
}
