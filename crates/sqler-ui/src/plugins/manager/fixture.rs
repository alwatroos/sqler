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
        _ => return PluginsStartup::unavailable(),
    };
    let backend: Arc<dyn PluginsBackend> = FakeBackend::new(snapshot);
    PluginsStartup::new(Some(backend), book)
}
