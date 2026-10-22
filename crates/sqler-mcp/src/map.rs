// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The mapping between `sqler-ai-tools` values and MCP wire values.

use serde_json::{Map, Value, json};
use sqler_ai_api::{ToolDefinition, ToolResult};
use sqler_ai_tools::names;

/// Tools that need the desktop app and are never exposed over MCP.
pub const UI_ONLY_TOOLS: [&str; 2] = [names::OPEN_IN_EDITOR, names::OPEN_RESULTS_TAB];

/// Whether a tool is one of the UI-only tools.
pub(crate) fn is_ui_only(name: &str) -> bool {
    UI_ONLY_TOOLS.contains(&name)
}

/// The definitions MCP exposes: the tool set's, minus the UI-only tools.
pub(crate) fn exposed(definitions: Vec<ToolDefinition>) -> Vec<ToolDefinition> {
    definitions
        .into_iter()
        .filter(|d| !is_ui_only(&d.name))
        .collect()
}

/// Maps a definition to an MCP `Tool` object, or `None` when `parameters` is not a JSON object
/// schema (`"type": "object"`), which MCP requires of `inputSchema`.
pub(crate) fn to_tool(definition: &ToolDefinition) -> Option<Value> {
    let schema = definition.parameters.as_object()?;
    if schema.get("type").and_then(Value::as_str) != Some("object") {
        return None;
    }
    Some(json!({
        "name": definition.name,
        "description": definition.description,
        "inputSchema": definition.parameters,
    }))
}

/// The raw argument text of a call: the object as JSON, or empty when the client sent none.
pub(crate) fn arguments_text(arguments: Option<&Map<String, Value>>) -> String {
    arguments
        .and_then(|a| serde_json::to_string(a).ok())
        .unwrap_or_default()
}

/// The MCP `CallToolResult` of a tool result: one text block with the enveloped content, unchanged.
pub(crate) fn to_call_result(result: &ToolResult) -> Value {
    json!({
        "content": [{"type": "text", "text": result.content}],
        "isError": result.is_error,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn every_built_in_definition_maps_to_a_tool() {
        for definition in sqler_ai_tools::definitions(true) {
            let tool = to_tool(&definition).unwrap();
            assert_eq!(tool["name"], definition.name.as_str());
            assert_eq!(tool["description"], definition.description.as_str());
            assert_eq!(tool["inputSchema"], definition.parameters);
        }
    }

    #[test]
    fn ui_only_tools_are_filtered() {
        let all = sqler_ai_tools::definitions(true);
        assert!(all.iter().any(|d| is_ui_only(&d.name)));
        let shown = exposed(all);
        assert!(shown.iter().all(|d| !is_ui_only(&d.name)));
        assert!(shown.iter().any(|d| d.name == names::RUN_QUERY));
    }

    #[test]
    fn non_object_schemas_are_refused() {
        for bad in [
            json!({"type": "string"}),
            json!([1]),
            json!({"properties": {}}),
        ] {
            assert!(to_tool(&ToolDefinition::new("x", "y", bad)).is_none());
        }
    }

    #[test]
    fn arguments_become_json_text() {
        assert_eq!(arguments_text(None), "");
        let args = json!({"a": 1}).as_object().cloned();
        assert_eq!(arguments_text(args.as_ref()), "{\"a\":1}");
    }

    #[test]
    fn results_keep_their_text_and_error_flag() {
        let ok = to_call_result(&ToolResult::ok("id", "<<text>>"));
        assert_eq!(ok["isError"], false);
        assert_eq!(ok["content"][0]["text"], "<<text>>");
        assert_eq!(ok["content"][0]["type"], "text");
        assert_eq!(ok["content"].as_array().unwrap().len(), 1);
        assert_eq!(
            to_call_result(&ToolResult::error("id", "bad"))["isError"],
            true
        );
    }
}
