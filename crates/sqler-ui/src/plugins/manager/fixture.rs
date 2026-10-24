// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Manifests and rows for the manager's tests and scenes.

use std::fmt::Write as _;
use std::sync::Arc;

use sqler_plugin_api::{ComponentDigest, Manifest, SettingSpec, SettingValue};

use crate::plugins::backend::{PluginRow, RowState};

/// A declarative manifest (no component) named after its id, with these capabilities.
#[must_use]
#[cfg(test)]
pub(crate) fn manifest(id: &str, caps: &[&str]) -> Manifest {
    named(id, &title_of(id), "1.0.0", caps, None, false)
}

#[cfg(test)]
fn title_of(id: &str) -> String {
    id.split('-')
        .map(|w| {
            let mut c = w.chars();
            c.next()
                .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A manifest with every field the manager shows. `entry` adds a component and a command.
#[must_use]
#[allow(clippy::expect_used)] // fixed fixture text
pub(crate) fn named(
    id: &str,
    name: &str,
    version: &str,
    caps: &[&str],
    description: Option<&str>,
    entry: bool,
) -> Manifest {
    let caps = caps
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let mut text = format!(
        "[plugin]\nid = \"{id}\"\nname = \"{name}\"\nversion = \"{version}\"\napi = \"0.1\"\n\
         capabilities = [{caps}]\n"
    );
    if let Some(description) = description {
        let _ = writeln!(text, "description = \"{description}\"");
    }
    if entry {
        text.push_str(
            "[entry]\ncomponent = \"bin/plugin.wasm\"\n\
             [[commands]]\nid = \"sync\"\ntitle = \"Sync now\"\n",
        );
    }
    Manifest::parse(&text).expect("fixture manifest parses")
}

/// An idle, enabled row; a manifest with an entry gets a component digest.
#[must_use]
pub(crate) fn row(manifest: Manifest) -> PluginRow {
    let component = manifest
        .entry()
        .is_some()
        .then(|| ComponentDigest::of(b"wasm"));
    PluginRow {
        manifest: Arc::new(manifest),
        component,
        dev: false,
        state: RowState::Idle,
        enabled: true,
        effective: sqler_plugin_api::CapabilitySet::new(),
        last_error: None,
        scope_issues: Vec::new(),
        settings: Vec::new(),
    }
}

/// [`row`] changed by `f`.
#[must_use]
#[cfg(test)]
pub(crate) fn row_with(manifest: Manifest, f: impl FnOnce(&mut PluginRow)) -> PluginRow {
    let mut r = row(manifest);
    f(&mut r);
    r
}

/// The row with an integer and a boolean setting.
#[must_use]
#[allow(clippy::expect_used)] // fixed fixture text
pub(crate) fn with_settings(mut r: PluginRow) -> PluginRow {
    let spec =
        |key: &str, label: &str, description: Option<&str>, default: SettingValue| SettingSpec {
            key: key.parse().expect("setting key"),
            label: label.to_owned(),
            description: description.map(str::to_owned),
            default,
        };
    r.settings = vec![
        (
            spec(
                "batch",
                "Batch size",
                Some("Rows per request."),
                SettingValue::Integer(100),
            ),
            SettingValue::Integer(100),
        ),
        (
            spec("verbose", "Verbose log", None, SettingValue::Boolean(false)),
            SettingValue::Boolean(false),
        ),
        (
            spec(
                "endpoint",
                "Endpoint",
                None,
                SettingValue::Text(String::new()),
            ),
            SettingValue::Text("https://reports.example".to_owned()),
        ),
    ];
    r
}

/// The plugin host of a screenshot scene: a scripted backend and a book in which some rows are decided.
///
/// `plugins-manager` shows every state the list has (running, off, denied, stopped, failed) and a folder that did
/// not load; `plugins-grant` has one new version of an approved plugin, so the dialog opens on its own;
/// `plugins-empty` finds nothing. Every other scene has no plugin support.
#[must_use]
#[allow(clippy::expect_used, clippy::too_many_lines)] // fixed fixture text, one arm per scene
pub(crate) fn scene_startup(scene: &str) -> crate::plugins::PluginsStartup {
    use crate::plugins::backend::{FakeBackend, PluginProblem, PluginSnapshot, PluginsBackend};
    use crate::plugins::grant::{GrantBook, GrantCommand};
    use crate::plugins::{PluginsStartup, RowState};

    let book = Arc::new(GrantBook::in_memory());
    let decide = |row: &PluginRow, command: GrantCommand| {
        let prompt = book
            .prompt(&row.manifest, row.component.as_ref())
            .expect("fixture prompt");
        book.decide(&prompt, command).expect("fixture decision");
    };
    let caps = |row: &PluginRow| row.manifest.capabilities().clone();
    let snapshot = match scene {
        "plugins-manager" => {
            let described = "Keeps a reporting database in step with your warehouse.";
            let mut acme = with_settings(row(named(
                "acme-sync",
                "Acme Sync",
                "1.3.0",
                &["db.read", "net.http:reports.example", "fs.read:data"],
                Some(described),
                true,
            )));
            acme.state = RowState::Live;
            decide(&acme, GrantCommand::Approve);
            acme.effective = caps(&acme);
            let mut counter = row(named(
                "row-counter",
                "Row Counter",
                "0.4.2",
                &["db.read"],
                Some("Counts the rows of the selected table."),
                false,
            ));
            counter.enabled = false;
            counter.state = RowState::Off;
            decide(&counter, GrantCommand::Approve);
            let exporter = row(named(
                "report-exporter",
                "Report Exporter",
                "2.0.0",
                &["db.read", "db.write", "fs.read:data"],
                None,
                true,
            ));
            decide(&exporter, GrantCommand::Deny);
            let mut linter = row(named(
                "flaky-linter",
                "Flaky Linter",
                "0.9.1",
                &["db.read"],
                None,
                true,
            ));
            linter.state = RowState::Faulted { count: 3 };
            linter.last_error =
                Some("The plugin ran out of time 3 times and was stopped.".to_owned());
            decide(&linter, GrantCommand::Approve);
            linter.effective = caps(&linter);
            let mut broken = row(named(
                "broken-driver",
                "Broken Driver",
                "1.0.0",
                &["db.read"],
                None,
                true,
            ));
            broken.state = RowState::LoadFailed;
            broken.last_error = Some(
                "The component could not be loaded: it is not a valid WebAssembly component."
                    .to_owned(),
            );
            decide(&broken, GrantCommand::Approve);
            PluginSnapshot {
                rows: vec![acme, counter, exporter, linter, broken],
                problems: vec![PluginProblem {
                    dir: "old-plugin".to_owned(),
                    id: None,
                    message: "plugin.toml: the plugin API version 9.0 is not supported".to_owned(),
                }],
            }
        }
        "plugins-grant" => {
            let older = row(named(
                "acme-sync",
                "Acme Sync",
                "1.2.0",
                &["db.read"],
                None,
                true,
            ));
            decide(&older, GrantCommand::Approve);
            let newer = row(named(
                "acme-sync",
                "Acme Sync",
                "1.3.0",
                &[
                    "db.read",
                    "db.write",
                    "net.http:*.xn--bcher-kva.example",
                    "fs.read:home/Library/Preferences",
                    "fs.read:data",
                ],
                Some("Keeps a reporting database in step with your warehouse."),
                true,
            ));
            PluginSnapshot {
                rows: vec![newer],
                problems: Vec::new(),
            }
        }
        "plugins-empty" => PluginSnapshot::default(),
        "plugins-panel" | "plugins-panel-error" | "plugins-panel-menu" => {
            let mut acme = row(contributing("acme-sync", "Acme Sync"));
            acme.state = RowState::Live;
            decide(&acme, GrantCommand::Approve);
            acme.effective = caps(&acme);
            // A second plugin with a status item on the right, to show both sides of the bar.
            let mut meter = row(named_with(
                "row-counter",
                "Row Counter",
                &["db.read"],
                "[[status-items]]\nid = \"rows\"\ntooltip = \"Rows in the selected table\"\n",
            ));
            meter.state = RowState::Live;
            decide(&meter, GrantCommand::Approve);
            meter.effective = caps(&meter);
            PluginSnapshot {
                rows: vec![acme, meter],
                problems: Vec::new(),
            }
        }
        "plugins-hello-panel" => {
            let mut hello = row(hello_panel());
            hello.state = RowState::Live;
            decide(&hello, GrantCommand::Approve);
            hello.effective = caps(&hello);
            PluginSnapshot {
                rows: vec![hello],
                problems: Vec::new(),
            }
        }
        _ => return PluginsStartup::unavailable(),
    };
    let fake = FakeBackend::new(snapshot);
    match scene {
        "plugins-panel" | "plugins-panel-menu" => {
            fake.script_panel("acme-sync", "overview", Ok(panel_tree()));
        }
        "plugins-panel-error" => fake.script_panel(
            "acme-sync",
            "overview",
            Err("The plugin ran out of time while drawing the panel.".to_owned()),
        ),
        "plugins-hello-panel" => {
            fake.script_panel("hello-panel", "hello", Ok(hello_panel_tree(3)));
            fake.script_status("hello-panel", "clicks", Some("Clicks: 3".to_owned()));
        }
        _ => {}
    }
    if scene.starts_with("plugins-panel") {
        fake.script_status("acme-sync", "env", Some("Acme: staging".to_owned()));
        fake.script_status("row-counter", "rows", Some("1,204 rows".to_owned()));
    }
    let backend: Arc<dyn PluginsBackend> = fake;
    PluginsStartup::new(Some(backend), book)
}

/// A manifest with these capabilities plus `body` (more tables), a component and nothing else.
#[must_use]
#[allow(clippy::expect_used)] // fixed fixture text
pub(crate) fn named_with(id: &str, name: &str, caps: &[&str], body: &str) -> Manifest {
    let caps = caps
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let text = format!(
        "[plugin]\nid = \"{id}\"\nname = \"{name}\"\nversion = \"1.0.0\"\napi = \"0.1\"\n\
         capabilities = [{caps}]\n[entry]\ncomponent = \"bin/plugin.wasm\"\n{body}"
    );
    Manifest::parse(&text).expect("fixture manifest parses")
}

/// A plugin that contributes one of everything: a palette command, an object and an editor menu command, a panel and
/// a status item on the left. It asks for `ui.panel`.
#[must_use]
pub(crate) fn contributing(id: &str, name: &str) -> Manifest {
    named_with(
        id,
        name,
        &["db.read", "ui.panel"],
        "[[commands]]\nid = \"sync\"\ntitle = \"Sync now\"\n\
         [[commands]]\nid = \"inspect\"\ntitle = \"Inspect table\"\n\
         [[commands]]\nid = \"format\"\ntitle = \"Normalize keywords\"\n\
         [[menus]]\ncommand = \"inspect\"\nlocation = \"object-browser\"\nobject-kinds = [\"table\", \"view\"]\n\
         [[menus]]\ncommand = \"format\"\nlocation = \"editor\"\n\
         [[panels]]\nid = \"overview\"\ntitle = \"Overview\"\n\
         [[status-items]]\nid = \"env\"\ntooltip = \"Warehouse environment\"\nalignment = \"left\"\n",
    )
}

/// The manifest of the `hello-panel` example plugin (`plugins/hello-panel/sqler.toml`): a command, a panel and a
/// status item on the left, asking for `ui.panel`.
#[must_use]
pub(crate) fn hello_panel() -> Manifest {
    named_with(
        "hello-panel",
        "Hello Panel",
        &["ui.panel"],
        "[[commands]]\nid = \"say-hello\"\ntitle = \"Say hello\"\n\
         [[panels]]\nid = \"hello\"\ntitle = \"Hello\"\n\
         [[status-items]]\nid = \"clicks\"\ntooltip = \"Clicks in the Hello panel\"\nalignment = \"left\"\n",
    )
}

/// The tree the `hello-panel` guest (`plugins/hello-panel/src/lib.rs`) draws after `n` clicks. The UI cannot run
/// wasm, so this is a copy of that tree.
#[must_use]
#[allow(clippy::expect_used)] // fixed fixture tree
pub(crate) fn hello_panel_tree(n: u32) -> sqler_plugin_api::WidgetTree {
    use sqler_plugin_api::{
        Button, ButtonStyle, Direction, Gap, Stack, Text, TextStyle, Widget, WidgetNode, WidgetTree,
    };
    let node = |widget: Widget, children: &[u32]| WidgetNode {
        widget,
        children: children.to_vec(),
    };
    let text = |content: String, style: TextStyle| Widget::Text(Text { content, style });
    let count = match n {
        0 => "Nobody has clicked yet.".to_owned(),
        1 => "Clicked once.".to_owned(),
        n => format!("Clicked {n} times."),
    };
    WidgetTree::new(vec![
        node(
            Widget::Stack(Stack {
                direction: Direction::Vertical,
                gap: Gap::Medium,
            }),
            &[1, 2, 3],
        ),
        node(
            text("Hello from a plugin".to_owned(), TextStyle::Heading),
            &[],
        ),
        node(text(count, TextStyle::Body), &[]),
        node(
            Widget::Button(Button {
                id: "click".to_owned(),
                label: "Click me".to_owned(),
                style: ButtonStyle::Primary,
            }),
            &[],
        ),
    ])
    .expect("fixture tree is valid")
}

/// The tree of the `overview` panel: a heading, a line of text, a table, a filter, a checkbox and two buttons.
#[must_use]
#[allow(clippy::expect_used)] // fixed fixture tree
pub(crate) fn panel_tree() -> sqler_plugin_api::WidgetTree {
    use sqler_plugin_api::{
        Button, ButtonStyle, Checkbox, Direction, Gap, Stack, Table, Text, TextInput, TextStyle,
        Widget, WidgetNode, WidgetTree,
    };
    let node = |widget: Widget, children: &[u32]| WidgetNode {
        widget,
        children: children.to_vec(),
    };
    let text = |content: &str, style: TextStyle| {
        node(
            Widget::Text(Text {
                content: content.to_owned(),
                style,
            }),
            &[],
        )
    };
    let rows = [
        ["customers", "12,408", "up to date"],
        ["orders", "88,231", "up to date"],
        ["order_items", "301,977", "behind by 2 batches"],
    ];
    WidgetTree::new(vec![
        node(
            Widget::Stack(Stack {
                direction: Direction::Vertical,
                gap: Gap::Medium,
            }),
            &[1, 2, 3, 4, 5, 6],
        ),
        text("Warehouse sync", TextStyle::Heading),
        text(
            "The last run finished 12 minutes ago. Two of three tables are up to date.",
            TextStyle::Muted,
        ),
        node(
            Widget::Table(Table {
                columns: ["Table", "Rows", "State"].map(str::to_owned).to_vec(),
                rows: rows
                    .iter()
                    .map(|r| r.iter().map(|c| (*c).to_owned()).collect())
                    .collect(),
            }),
            &[],
        ),
        node(
            Widget::TextInput(TextInput {
                id: "filter".to_owned(),
                value: String::new(),
                placeholder: Some("Filter tables".to_owned()),
            }),
            &[],
        ),
        node(
            Widget::Checkbox(Checkbox {
                id: "auto".to_owned(),
                label: "Sync after every schema change".to_owned(),
                checked: true,
            }),
            &[],
        ),
        node(
            Widget::Stack(Stack {
                direction: Direction::Horizontal,
                gap: Gap::Small,
            }),
            &[7, 8],
        ),
        node(
            Widget::Button(Button {
                id: "sync".to_owned(),
                label: "Sync now".to_owned(),
                style: ButtonStyle::Primary,
            }),
            &[],
        ),
        node(
            Widget::Button(Button {
                id: "log".to_owned(),
                label: "Open log".to_owned(),
                style: ButtonStyle::Secondary,
            }),
            &[],
        ),
    ])
    .expect("fixture tree is valid")
}

/// A declarative host-driver manifest: plugin `plugin`, one driver `driver` that declares `dialect`.
#[must_use]
#[cfg(test)]
#[allow(clippy::expect_used)] // fixed fixture text
pub(crate) fn driver_manifest(plugin: &str, driver: &str, dialect: Option<&str>) -> Manifest {
    let mut text = format!(
        "[plugin]\nid = \"{plugin}\"\nname = \"Driver\"\nversion = \"0.1.0\"\napi = \"0.1\"\n\
         capabilities = [\"driver.host:mssql\"]\n\
         [[drivers]]\nid = \"{driver}\"\nname = \"Driver\"\nkind = \"host\"\nengine = \"mssql\"\n"
    );
    if let Some(dialect) = dialect {
        let _ = writeln!(text, "dialect = \"{dialect}\"");
    }
    Manifest::parse(&text).expect("driver manifest parses")
}
