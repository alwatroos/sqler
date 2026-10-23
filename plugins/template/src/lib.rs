// SPDX-License-Identifier: Apache-2.0

//! A SQLer plugin (`ui-plugin` world): the command `say-hello` notifies and updates the status item `greetings`.

// The wit-bindgen macro expands to `unsafe` FFI glue and undocumented items, so the generated code and the exports
// live in one module with the lints relaxed.
#[allow(unsafe_code, missing_docs, clippy::all, clippy::pedantic)]
mod bindings {
    wit_bindgen::generate!({
        path: "wit",
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
    use self::sqler::plugin::widgets::{Text, TextStyle, Widget, WidgetNode};

    /// How often `say-hello` ran. A plugin instance lives as long as the plugin is loaded, so a static is enough.
    static GREETINGS: AtomicU32 = AtomicU32::new(0);

    struct Plugin;

    impl lifecycle::Guest for Plugin {
        fn init() -> Result<(), lifecycle::PluginError> {
            write(Level::Info, &format!("{}: init", env!("CARGO_PKG_NAME")));
            Ok(())
        }
    }

    fn status_text(n: u32) -> String {
        format!("Hellos: {n}")
    }

    impl Guest for Plugin {
        fn run_command(command: String, _context: CommandContext) -> Result<(), PluginError> {
            if command != "say-hello" {
                return Err(PluginError {
                    code: ErrorCode::NotFound,
                    message: format!("unknown command {command}"),
                });
            }
            let n = GREETINGS.fetch_add(1, Ordering::Relaxed) + 1;
            ui::notify("Hello from a plugin!");
            ui::set_status("greetings", &status_text(n));
            Ok(())
        }

        fn render_panel(_panel: String) -> WidgetTree {
            WidgetTree {
                nodes: vec![WidgetNode {
                    widget: Widget::Text(Text {
                        content: "No panel yet.".to_owned(),
                        style: TextStyle::Body,
                    }),
                    children: Vec::new(),
                }],
            }
        }

        fn panel_event(_panel: String, _event: WidgetEvent) -> Option<WidgetTree> {
            None
        }

        fn status(item: String) -> Option<String> {
            (item == "greetings").then(|| status_text(GREETINGS.load(Ordering::Relaxed)))
        }
    }

    export!(Plugin);
}
