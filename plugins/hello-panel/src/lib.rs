// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Example plugin `hello-panel` (`ui-plugin` world). It shows the three UI contributions a plugin can make: a panel
//! with a heading, a click counter and a button; a command that notifies and asks the host to redraw the panel; and a
//! status item that shows the count. It is the target of the "write your first plugin" tutorial, so it stays small and
//! carries no behaviour that exists only for tests.

// The wit-bindgen macro expands to `unsafe` FFI glue and undocumented items (AGENTS.md section 4 exception),
// so the generated code and the exports live in one module with the lints relaxed.
#[allow(unsafe_code, missing_docs, clippy::all, clippy::pedantic)]
mod bindings {
    wit_bindgen::generate!({
        path: "../../crates/sqler-plugin-api/wit",
        world: "ui-plugin",
    });

    use std::sync::atomic::{AtomicU32, Ordering};

    use self::exports::sqler::plugin::lifecycle;
    use self::exports::sqler::plugin::ui_handler::{
        CommandContext, Guest, PluginError, WidgetEvent, WidgetTree,
    };
    use self::sqler::plugin::log::{Level, write};
    use self::sqler::plugin::types::ErrorCode;
    use self::sqler::plugin::ui;
    use self::sqler::plugin::widgets::{
        Button, ButtonStyle, Direction, Gap, Stack, Text, TextStyle, Widget, WidgetNode,
    };

    /// How often the button was clicked. A plugin instance lives as long as the plugin is loaded, so a static is
    /// enough for a counter.
    static CLICKS: AtomicU32 = AtomicU32::new(0);

    struct Hello;

    impl lifecycle::Guest for Hello {
        fn init() -> Result<(), lifecycle::PluginError> {
            write(Level::Info, "hello-panel: init");
            Ok(())
        }
    }

    fn count_text(n: u32) -> String {
        match n {
            0 => "Nobody has clicked yet.".to_owned(),
            1 => "Clicked once.".to_owned(),
            n => format!("Clicked {n} times."),
        }
    }

    fn status_text(n: u32) -> String {
        format!("Clicks: {n}")
    }

    fn text(content: String, style: TextStyle) -> WidgetNode {
        WidgetNode {
            widget: Widget::Text(Text { content, style }),
            children: Vec::new(),
        }
    }

    /// The panel: node 0 is the root, the other nodes are its children in order.
    fn tree(n: u32) -> WidgetTree {
        WidgetTree {
            nodes: vec![
                WidgetNode {
                    widget: Widget::Stack(Stack {
                        direction: Direction::Vertical,
                        gap: Gap::Medium,
                    }),
                    children: vec![1, 2, 3],
                },
                text("Hello from a plugin".to_owned(), TextStyle::Heading),
                text(count_text(n), TextStyle::Body),
                WidgetNode {
                    widget: Widget::Button(Button {
                        id: "click".to_owned(),
                        label: "Click me".to_owned(),
                        style: ButtonStyle::Primary,
                    }),
                    children: Vec::new(),
                },
            ],
        }
    }

    impl Guest for Hello {
        fn run_command(command: String, _context: CommandContext) -> Result<(), PluginError> {
            if command != "say-hello" {
                return Err(PluginError {
                    code: ErrorCode::NotFound,
                    message: format!("unknown command {command}"),
                });
            }
            ui::notify("Hello!");
            ui::refresh_panel("hello");
            Ok(())
        }

        fn render_panel(_panel: String) -> WidgetTree {
            tree(CLICKS.load(Ordering::Relaxed))
        }

        fn panel_event(panel: String, event: WidgetEvent) -> Option<WidgetTree> {
            match (panel.as_str(), event) {
                ("hello", WidgetEvent::Clicked(id)) if id == "click" => {
                    let n = CLICKS.fetch_add(1, Ordering::Relaxed) + 1;
                    ui::set_status("clicks", &status_text(n));
                    Some(tree(n))
                }
                _ => None,
            }
        }

        fn status(item: String) -> Option<String> {
            (item == "clicks").then(|| status_text(CLICKS.load(Ordering::Relaxed)))
        }
    }

    export!(Hello);
}
