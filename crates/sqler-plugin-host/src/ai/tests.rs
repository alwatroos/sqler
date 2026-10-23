// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Unit tests of the tool listing and the checks that run before a plugin is instantiated. The real guest is driven
//! by `tests/guests_ai.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use sqler_plugin_api::{CapabilitySet, GrantKey, Limits, PluginId, ToolName};

use super::{MAX_AI_ARGUMENT_BYTES, PluginAiTool};
use crate::egress::NoEgress;
use crate::error::{CallError, ErrorCode};
use crate::grant::GrantSource;
use crate::query::{DataError, DataService, PluginConnection, PluginData};
use crate::runtime::{HostOptions, PluginHost, Services};
use crate::settings::MemorySettings;
use crate::status::PluginState;
use crate::{PluginDirs, discover};

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Approves what each key was given.
#[derive(Default)]
struct Approve(Mutex<Vec<(GrantKey, CapabilitySet)>>);

impl GrantSource for Approve {
    fn approved(&self, key: &GrantKey) -> Option<CapabilitySet> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, s)| s.clone())
    }
}

/// A data service that lends nothing.
struct NoData;

impl DataService for NoData {
    fn connections(&self) -> Vec<PluginConnection> {
        Vec::new()
    }

    fn catalog(&self, _connection: &str) -> Option<sqler_catalog::Catalog> {
        None
    }

    fn acquire<'a>(
        &'a self,
        connection: &'a str,
    ) -> sqler_driver_api::BoxFuture<'a, Result<Box<dyn sqler_driver_api::Session>, DataError>>
    {
        Box::pin(async move { Err(DataError::UnknownConnection(connection.to_owned())) })
    }
}

fn data() -> PluginData {
    PluginData::new(Arc::new(NoData))
}

struct Fixture {
    _scratch: Scratch,
    rt: tokio::runtime::Runtime,
    host: PluginHost,
}

/// `plugins`: (id, capabilities, tools). Every plugin runs the `ok` component and is approved what it declares.
#[allow(clippy::format_push_string)]
fn fixture(test: &str, plugins: &[(&str, &[&str], &[&str])]) -> Fixture {
    let root = std::env::temp_dir().join(format!("sqler-host-ai-{}-{test}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let scratch = Scratch(root.clone());
    let mut dirs = Vec::new();
    for (id, caps, tools) in plugins {
        let dir = root.join(id);
        fs::create_dir_all(&dir).unwrap();
        let caps = caps
            .iter()
            .map(|c| format!("\"{c}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let mut manifest = format!(
            "[plugin]\nid = \"{id}\"\nname = \"Name of {id}\"\nversion = \"1.0.0\"\napi = \"0.1\"\ncapabilities = [{caps}]\n\n[entry]\ncomponent = \"plugin.wasm\"\n"
        );
        for tool in *tools {
            manifest.push_str(&format!(
                "\n[[ai-tools]]\nname = \"{tool}\"\ndescription = \"d\"\n[ai-tools.input-schema]\ntype = \"object\"\n"
            ));
        }
        fs::write(dir.join("sqler.toml"), manifest).unwrap();
        let wasm = wat::parse_str(include_str!("../../tests/fixtures/wat/ok.wat")).unwrap();
        fs::write(dir.join("plugin.wasm"), wasm).unwrap();
        dirs.push(dir);
    }
    let d = discover(&PluginDirs {
        dev: dirs,
        ..PluginDirs::default()
    });
    assert!(d.problems.is_empty(), "{:?}", d.problems);
    let grants = Approve(Mutex::new(
        d.plugins
            .iter()
            .map(|p| (p.key.clone(), p.manifest.capabilities().clone()))
            .collect(),
    ));
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let host = PluginHost::new(HostOptions {
        runtime: rt.handle().clone(),
        grants: Arc::new(grants),
        settings: Arc::new(MemorySettings::default()),
        egress: Arc::new(NoEgress),
        limits: Limits::DEFAULT,
        services: Services::new(),
    });
    assert_eq!(host.load(d).len(), 0);
    Fixture {
        _scratch: scratch,
        rt,
        host,
    }
}

fn id(s: &str) -> PluginId {
    s.parse().unwrap()
}

fn tool(s: &str) -> ToolName {
    s.parse().unwrap()
}

#[test]
fn model_name_joins_plugin_and_tool() {
    let f = fixture("name", &[("alpha", &["ai.tool"], &["echo"])]);
    let tools = f.host.ai_tools();
    assert_eq!(tools.len(), 1);
    let t: &PluginAiTool = &tools[0];
    assert_eq!(t.model_name(), "alpha__echo");
    assert_eq!(t.plugin_name, "Name of alpha");
    assert_eq!(t.tool.name, tool("echo"));
    f.host.shutdown();
}

#[test]
fn only_plugins_with_an_effective_grant_are_listed_and_nothing_is_started() {
    let f = fixture(
        "listing",
        &[
            ("alpha", &["ai.tool"], &["one", "two"]),
            ("beta", &["db.read"], &[]),
            ("gamma", &["ai.tool"], &["three"]),
        ],
    );
    let names: Vec<String> = f
        .host
        .ai_tools()
        .iter()
        .map(PluginAiTool::model_name)
        .collect();
    assert_eq!(names, ["alpha__one", "alpha__two", "gamma__three"]);
    assert!(f.host.is_idle(), "listing must not touch the engine");
    f.host.disable(&id("gamma"));
    assert!(matches!(
        f.host.plugins()[2].state,
        PluginState::Disabled { .. }
    ));
    let names: Vec<String> = f
        .host
        .ai_tools()
        .iter()
        .map(PluginAiTool::model_name)
        .collect();
    assert_eq!(names, ["alpha__one", "alpha__two"]);
    f.host.shutdown();
}

#[test]
fn a_model_name_over_64_bytes_is_left_out() {
    let long_tool = "t".repeat(48);
    let plugin = "p".repeat(20);
    let f = fixture(
        "long",
        &[(
            plugin.as_str(),
            &["ai.tool"],
            &[long_tool.as_str(), "short"],
        )],
    );
    let names: Vec<String> = f
        .host
        .ai_tools()
        .iter()
        .map(PluginAiTool::model_name)
        .collect();
    assert_eq!(names, [format!("{plugin}__short")]);
    f.host.shutdown();
}

#[test]
fn invoke_refuses_before_instantiating() {
    let f = fixture(
        "refusals",
        &[
            ("alpha", &["ai.tool"], &["echo"]),
            ("beta", &["db.read"], &[]),
        ],
    );
    let invoke = |plugin: &str, name: &str, args: String| {
        f.rt.block_on(
            f.host
                .invoke_ai_tool(&id(plugin), &tool(name), args, data()),
        )
    };
    let code = |r: Result<String, CallError>| match r {
        Err(CallError::Guest(g)) => g.code,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        code(invoke(
            "alpha",
            "echo",
            "x".repeat(MAX_AI_ARGUMENT_BYTES + 1)
        )),
        ErrorCode::InvalidArgument
    );
    assert_eq!(
        invoke("nobody", "echo", "{}".to_owned()),
        Err(CallError::NotFound)
    );
    assert_eq!(
        code(invoke("alpha", "other", "{}".to_owned())),
        ErrorCode::NotFound
    );
    assert!(f.host.is_idle(), "refused calls must not touch the engine");
    f.host.shutdown();
}

#[cfg(feature = "cl")]
#[test]
fn a_component_without_the_export_is_missing_export() {
    let f = fixture("missing", &[("alpha", &["ai.tool"], &["echo"])]);
    let e =
        f.rt.block_on(
            f.host
                .invoke_ai_tool(&id("alpha"), &tool("echo"), "{}".to_owned(), data()),
        )
        .unwrap_err();
    assert_eq!(
        e,
        CallError::MissingExport {
            interface: "ai-tools"
        }
    );
    // A missing export is not a fault: the instance stays.
    assert_eq!(f.host.plugins()[0].state, PluginState::Live);
    f.host.shutdown();
}
