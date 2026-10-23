// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Conversions between the generated `widgets` types and the API types.

use sqler_plugin_api::{
    Button, ButtonStyle, Checkbox, ColumnRef, CommandContext, Direction, Gap, ObjectRef, Select,
    Stack, Table, Text, TextInput, TextStyle, Widget, WidgetError, WidgetEvent, WidgetNode,
    WidgetTree,
};

use crate::query::{data_type_to_wit, kind_to_wit};
use crate::runtime::wit::{types as wit_types, widgets as wit};

fn direction(d: wit::Direction) -> Direction {
    match d {
        wit::Direction::Vertical => Direction::Vertical,
        wit::Direction::Horizontal => Direction::Horizontal,
    }
}

fn gap(g: wit::Gap) -> Gap {
    match g {
        wit::Gap::None => Gap::None,
        wit::Gap::Small => Gap::Small,
        wit::Gap::Medium => Gap::Medium,
        wit::Gap::Large => Gap::Large,
    }
}

fn text_style(s: wit::TextStyle) -> TextStyle {
    match s {
        wit::TextStyle::Body => TextStyle::Body,
        wit::TextStyle::Muted => TextStyle::Muted,
        wit::TextStyle::Heading => TextStyle::Heading,
        wit::TextStyle::Code => TextStyle::Code,
    }
}

fn button_style(s: wit::ButtonStyle) -> ButtonStyle {
    match s {
        wit::ButtonStyle::Primary => ButtonStyle::Primary,
        wit::ButtonStyle::Secondary => ButtonStyle::Secondary,
    }
}

fn widget(w: wit::Widget) -> Widget {
    match w {
        wit::Widget::Stack(s) => Widget::Stack(Stack {
            direction: direction(s.direction),
            gap: gap(s.gap),
        }),
        wit::Widget::Text(t) => Widget::Text(Text {
            content: t.content,
            style: text_style(t.style),
        }),
        wit::Widget::Button(b) => Widget::Button(Button {
            id: b.id,
            label: b.label,
            style: button_style(b.style),
        }),
        wit::Widget::TextInput(t) => Widget::TextInput(TextInput {
            id: t.id,
            value: t.value,
            placeholder: t.placeholder,
        }),
        wit::Widget::Checkbox(c) => Widget::Checkbox(Checkbox {
            id: c.id,
            label: c.label,
            checked: c.checked,
        }),
        wit::Widget::Select(s) => Widget::Select(Select {
            id: s.id,
            options: s.options,
            selected: s.selected,
        }),
        wit::Widget::Table(t) => Widget::Table(Table {
            columns: t.columns,
            rows: t.rows,
        }),
        wit::Widget::Divider => Widget::Divider,
    }
}

/// The validated tree of a guest's answer; a bad tree is the validation error.
pub(crate) fn tree_from_wit(tree: wit::WidgetTree) -> Result<WidgetTree, WidgetError> {
    WidgetTree::new(
        tree.nodes
            .into_iter()
            .map(|n| WidgetNode {
                widget: widget(n.widget),
                children: n.children,
            })
            .collect(),
    )
}

/// The WIT event for a click or edit in a panel.
pub(crate) fn event_to_wit(event: WidgetEvent) -> wit::WidgetEvent {
    match event {
        WidgetEvent::Clicked(id) => wit::WidgetEvent::Clicked(id),
        WidgetEvent::Changed { id, value } => {
            wit::WidgetEvent::Changed(wit::FieldChange { id, value })
        }
        WidgetEvent::Toggled { id, checked } => {
            wit::WidgetEvent::Toggled(wit::Toggle { id, checked })
        }
        WidgetEvent::Selected { id, index } => {
            wit::WidgetEvent::Selected(wit::Selection { id, index })
        }
    }
}

fn object_to_wit(o: ObjectRef) -> Option<wit_types::DbObject> {
    Some(wit_types::DbObject {
        kind: kind_to_wit(o.kind)?,
        parent: wit_types::ObjectPath {
            database: o.parent.database,
            schema: o.parent.schema,
            table: o.parent.table,
        },
        name: o.name,
    })
}

fn column_to_wit(c: ColumnRef) -> wit_types::Column {
    wit_types::Column {
        name: c.name,
        data_type: data_type_to_wit(c.data_type),
        type_name: c.type_name,
    }
}

/// The WIT context, or `None` when it holds an object kind or context this host cannot express.
pub(crate) fn context_to_wit(context: CommandContext) -> Option<wit::CommandContext> {
    Some(match context {
        CommandContext::None => wit::CommandContext::None,
        CommandContext::Editor {
            connection,
            selection,
        } => wit::CommandContext::Editor(wit::EditorContext {
            connection,
            selection,
        }),
        CommandContext::Object { connection, object } => {
            wit::CommandContext::Object(wit::ObjectContext {
                connection,
                object: object_to_wit(object)?,
            })
        }
        CommandContext::Results {
            connection,
            columns,
        } => wit::CommandContext::Results(wit::ResultsContext {
            connection,
            columns: columns.into_iter().map(column_to_wit).collect(),
        }),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use sqler_driver_api::sqler_core::{DataType, ObjectKind};
    use sqler_plugin_api::ObjectPathRef;

    use super::*;

    fn node(widget: wit::Widget, children: Vec<u32>) -> wit::WidgetNode {
        wit::WidgetNode { widget, children }
    }

    fn sample() -> wit::WidgetTree {
        wit::WidgetTree {
            nodes: vec![
                node(
                    wit::Widget::Stack(wit::Stack {
                        direction: wit::Direction::Horizontal,
                        gap: wit::Gap::Large,
                    }),
                    vec![1, 2, 3, 4, 5, 6, 7],
                ),
                node(
                    wit::Widget::Text(wit::Text {
                        content: "hi".into(),
                        style: wit::TextStyle::Code,
                    }),
                    vec![],
                ),
                node(
                    wit::Widget::Button(wit::Button {
                        id: "go".into(),
                        label: "Go".into(),
                        style: wit::ButtonStyle::Primary,
                    }),
                    vec![],
                ),
                node(
                    wit::Widget::TextInput(wit::TextInput {
                        id: "name".into(),
                        value: "v".into(),
                        placeholder: Some("p".into()),
                    }),
                    vec![],
                ),
                node(
                    wit::Widget::Checkbox(wit::Checkbox {
                        id: "on".into(),
                        label: "On".into(),
                        checked: true,
                    }),
                    vec![],
                ),
                node(
                    wit::Widget::Select(wit::Select {
                        id: "pick".into(),
                        options: vec!["a".into(), "b".into()],
                        selected: Some(1),
                    }),
                    vec![],
                ),
                node(
                    wit::Widget::Table(wit::Table {
                        columns: vec!["c".into()],
                        rows: vec![vec!["1".into()]],
                    }),
                    vec![],
                ),
                node(wit::Widget::Divider, vec![]),
            ],
        }
    }

    #[test]
    fn a_valid_tree_converts_field_for_field() {
        let tree = tree_from_wit(sample()).unwrap();
        assert_eq!(tree.nodes().len(), 8);
        assert_eq!(
            tree.root().widget,
            Widget::Stack(Stack {
                direction: Direction::Horizontal,
                gap: Gap::Large
            })
        );
        assert_eq!(tree.root().children, vec![1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(
            tree.nodes()[3].widget,
            Widget::TextInput(TextInput {
                id: "name".into(),
                value: "v".into(),
                placeholder: Some("p".into())
            })
        );
        assert_eq!(
            tree.nodes()[5].widget,
            Widget::Select(Select {
                id: "pick".into(),
                options: vec!["a".into(), "b".into()],
                selected: Some(1)
            })
        );
        assert_eq!(tree.nodes()[7].widget, Widget::Divider);
    }

    #[test]
    fn a_bad_tree_is_an_error_value() {
        assert_eq!(
            tree_from_wit(wit::WidgetTree { nodes: vec![] }).unwrap_err(),
            WidgetError::Empty
        );
        let mut dup = sample();
        dup.nodes[3] = node(
            wit::Widget::Button(wit::Button {
                id: "go".into(),
                label: "again".into(),
                style: wit::ButtonStyle::Secondary,
            }),
            vec![],
        );
        assert_eq!(
            tree_from_wit(dup).unwrap_err(),
            WidgetError::DuplicateElementId
        );
        let leaf_with_child = wit::WidgetTree {
            nodes: vec![node(wit::Widget::Divider, vec![0])],
        };
        assert_eq!(
            tree_from_wit(leaf_with_child).unwrap_err(),
            WidgetError::ChildrenOnLeaf
        );
    }

    #[test]
    fn events_map_to_their_wit_cases() {
        assert!(matches!(
            event_to_wit(WidgetEvent::Clicked("a".into())),
            wit::WidgetEvent::Clicked(id) if id == "a"
        ));
        assert!(matches!(
            event_to_wit(WidgetEvent::Changed { id: "a".into(), value: "v".into() }),
            wit::WidgetEvent::Changed(c) if c.id == "a" && c.value == "v"
        ));
        assert!(matches!(
            event_to_wit(WidgetEvent::Toggled { id: "a".into(), checked: true }),
            wit::WidgetEvent::Toggled(t) if t.id == "a" && t.checked
        ));
        assert!(matches!(
            event_to_wit(WidgetEvent::Selected { id: "a".into(), index: 2 }),
            wit::WidgetEvent::Selected(s) if s.id == "a" && s.index == 2
        ));
    }

    #[test]
    fn contexts_map_to_their_wit_cases() {
        assert!(matches!(
            context_to_wit(CommandContext::None),
            Some(wit::CommandContext::None)
        ));
        let editor = context_to_wit(CommandContext::Editor {
            connection: Some("c1".into()),
            selection: "select 1".into(),
        });
        assert!(matches!(
            editor,
            Some(wit::CommandContext::Editor(e)) if e.connection.as_deref() == Some("c1") && e.selection == "select 1"
        ));
        let object = context_to_wit(CommandContext::Object {
            connection: "c1".into(),
            object: ObjectRef {
                kind: ObjectKind::Table,
                parent: ObjectPathRef {
                    database: String::new(),
                    schema: "main".into(),
                    table: None,
                },
                name: "t".into(),
            },
        });
        match object {
            Some(wit::CommandContext::Object(o)) => {
                assert_eq!(o.connection, "c1");
                assert_eq!(o.object.name, "t");
                assert_eq!(o.object.parent.database, "");
                assert_eq!(o.object.parent.schema, "main");
                assert!(matches!(o.object.kind, wit_types::ObjectKind::Table));
            }
            other => panic!("unexpected {other:?}"),
        }
        let results = context_to_wit(CommandContext::Results {
            connection: "c1".into(),
            columns: vec![ColumnRef {
                name: "id".into(),
                data_type: DataType::Integer,
                type_name: "INTEGER".into(),
            }],
        });
        match results {
            Some(wit::CommandContext::Results(r)) => {
                assert_eq!(r.columns.len(), 1);
                assert_eq!(r.columns[0].type_name, "INTEGER");
                assert!(matches!(
                    r.columns[0].data_type,
                    wit_types::DataType::Integer
                ));
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}
