// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! External tools: how a host adds tools (for example from plugins) to a [`ToolSet`](crate::ToolSet).
//!
//! The host implements [`ExternalTools`]. The tool set lists the tools after its own, checks the
//! arguments against a small subset of JSON Schema and dispatches the call through the same exit
//! as every built-in tool, so redaction, the untrusted-data envelope, the audit row and the
//! cancel guard apply with no extra code. A tool that wants to run a statement that needs
//! approval asks the surface's own [`Approver`](crate::Approver) through [`ExternalContext::approve`]; one denial,
//! or the lack of an approver, turns the whole call into an error whatever the tool returns.

use std::sync::{Arc, Mutex, PoisonError};

use serde_json::Value;
use sqler_ai_api::BoxFuture;
use sqler_driver_api::sqler_core::{Environment, Value as SqlValue};
use sqler_sql::StatementClass;

use crate::approve::{ApprovalReason, ApprovalRequest, ApprovalStage, Decision};
use crate::audit::Surface;
use crate::error::ToolError;
use crate::schema;
use crate::source::ConnectionInfo;
use crate::toolset::{ExecutedStatement, Inner, Phase, Ran};

/// The `ApprovalRequest::tool` of every statement an external tool asks to run.
pub const EXTERNAL_TOOL: &str = "external_tool";

/// What a model-facing external tool name must contain, between the plugin and the tool.
pub const EXTERNAL_NAME_SEPARATOR: &str = "__";

/// The longest description of an external tool, in bytes; longer ones are clipped.
pub const MAX_EXTERNAL_DESCRIPTION_BYTES: usize = 1024;

/// The longest tool name.
const MAX_NAME_BYTES: usize = 64;

/// The longest argument name quoted in an error message.
const MAX_KEY_IN_MESSAGE: usize = 64;

/// One tool a host adds to a [`ToolSet`](crate::ToolSet).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ExternalTool {
    /// The name the model uses: `[A-Za-z0-9_-]{1,64}` with `__` in it, and never a built-in name.
    pub name: String,
    /// What the tool does. Clipped to [`MAX_EXTERNAL_DESCRIPTION_BYTES`] when listed.
    pub description: String,
    /// The JSON Schema of the arguments. Must be an object with `"type":"object"`.
    pub input_schema: Value,
}

impl ExternalTool {
    /// Describes a tool.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: Value,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            input_schema,
        }
    }
}

/// A host that supplies extra tools to a [`ToolSet`](crate::ToolSet).
pub trait ExternalTools: Send + Sync {
    /// The tools on offer now. The tool set filters them (see [`ExternalTool::name`]).
    fn tools(&self) -> Vec<ExternalTool>;

    /// Runs one tool. `arguments` already passed the schema check. The result is redacted,
    /// wrapped and audited by the tool set; a denial recorded on `ctx` wins over the outcome.
    fn call<'a>(
        &'a self,
        name: &'a str,
        arguments: Value,
        ctx: ExternalContext,
    ) -> BoxFuture<'a, Result<Value, ToolError>>;
}

/// One statement an external tool asks the surface's approver to confirm.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ExternalApproval {
    /// The id of the connection the statement would run on.
    pub connection: String,
    /// The exact SQL text.
    pub sql: String,
    /// Bound parameters. A statement with parameters is refused: the card cannot show them.
    pub params: Vec<SqlValue>,
    /// The class of the statement.
    pub class: StatementClass,
    /// Why approval is needed.
    pub reason: ApprovalReason,
    /// Which confirmation this is.
    pub stage: ApprovalStage,
}

impl ExternalApproval {
    /// Describes one approval question.
    #[must_use]
    pub fn new(
        connection: impl Into<String>,
        sql: impl Into<String>,
        params: Vec<SqlValue>,
        class: StatementClass,
        reason: ApprovalReason,
        stage: ApprovalStage,
    ) -> Self {
        Self {
            connection: connection.into(),
            sql: sql.into(),
            params,
            class,
            reason,
            stage,
        }
    }
}

/// What the tool set knows about one external call.
#[derive(Debug, Default)]
struct ExtState {
    /// Index of the statement that was refused; later asks are denied unasked.
    denied: Option<usize>,
    /// A statement needed approval and the surface has no approver.
    no_approver: bool,
    /// The number of `Confirm` stages asked so far.
    confirms: usize,
    /// The call is over; nothing more is accepted.
    closed: bool,
}

/// Closes an [`ExternalContext`] when dropped.
pub(crate) struct CloseOnDrop(ExternalContext);

impl Drop for CloseOnDrop {
    fn drop(&mut self) {
        self.0.state(|s| s.closed = true);
    }
}

/// The view of one call that a tool gets: the surface's connections and its approver.
///
/// Cheap to clone and `'static`. It is valid for the one call it was made for.
#[derive(Clone)]
pub struct ExternalContext {
    inner: Arc<Inner>,
    ran: Ran,
    state: Arc<Mutex<ExtState>>,
}

impl std::fmt::Debug for ExternalContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExternalContext")
            .field("surface", &self.inner.surface)
            .field("approver", &self.inner.approver.is_some())
            .finish_non_exhaustive()
    }
}

impl ExternalContext {
    pub(crate) fn new(inner: Arc<Inner>, ran: Ran) -> Self {
        Self {
            inner,
            ran,
            state: Arc::default(),
        }
    }

    fn state<R>(&self, f: impl FnOnce(&mut ExtState) -> R) -> R {
        f(&mut self.state.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// A guard that closes the context when dropped (the call finished, was cancelled or timed
    /// out), so a leaked clone cannot approve or lend connections afterwards.
    pub(crate) fn close_on_drop(&self) -> CloseOnDrop {
        CloseOnDrop(self.clone())
    }

    /// Ends the call: what is decided after the tool returned.
    pub(crate) fn verdict(&self) -> Result<(), ToolError> {
        self.state(|s| {
            s.closed = true;
            if let Some(index) = s.denied {
                Err(ToolError::Denied { index })
            } else if s.no_approver {
                Err(ToolError::NoApprover)
            } else {
                Ok(())
            }
        })
    }

    /// The surface that makes this call.
    #[must_use]
    pub fn surface(&self) -> Surface {
        self.inner.surface
    }

    /// The connections the surface lends.
    #[must_use]
    pub fn connections(&self) -> Vec<ConnectionInfo> {
        self.inner.source.connections()
    }

    /// The connection with this id (ids only, not display names). The call records it, which
    /// feeds the audit row and the redaction.
    ///
    /// # Errors
    /// [`ToolError::UnknownConnection`] if the surface does not lend it.
    pub fn use_connection(&self, id: &str) -> Result<ConnectionInfo, ToolError> {
        if self.state(|s| s.closed) {
            return Err(ToolError::NotAvailable("the call has ended".to_owned()));
        }
        let info = self
            .connections()
            .into_iter()
            .find(|c| c.id == id)
            .ok_or_else(|| ToolError::UnknownConnection(id.to_owned()))?;
        self.record(&info);
        Ok(info)
    }

    /// Whether the surface can ask anyone.
    #[must_use]
    pub fn has_approver(&self) -> bool {
        self.inner.approver.is_some()
    }

    /// Records the connection of the call: the first one, then only one that withholds rows.
    fn record(&self, info: &ConnectionInfo) {
        self.ran.with(|s| {
            let replace = match &s.info {
                None => true,
                Some(current) => current.id != info.id && !info.send_rows,
            };
            if replace {
                s.connection = Some(info.id.clone());
                s.info = Some(info.clone());
            }
        });
    }

    /// Notes a refused statement: planned only, so the audit shows it as not run.
    fn plan_refused(&self, sql: &str, class: StatementClass) -> usize {
        self.ran.with(|s| {
            let index = s.planned.len();
            s.planned.push((sql.to_owned(), class, true));
            index
        })
    }

    /// Notes a refused statement and marks the call denied at it (the first denial stands).
    fn refuse(&self, sql: &str, class: StatementClass) -> usize {
        let index = self.plan_refused(sql, class);
        self.state(|s| {
            if s.denied.is_none() {
                s.denied = Some(index);
            }
        });
        index
    }

    /// Asks the surface's approver whether `request` may run. The answer is final: a tool that
    /// runs a statement after `Deny` still gets an error result from the tool set.
    ///
    /// Order: an earlier denial, an unknown connection, bound parameters and a missing approver
    /// all give `Deny`; only then is the approver asked.
    pub fn approve(&self, request: ExternalApproval) -> BoxFuture<'static, Decision> {
        let this = self.clone();
        Box::pin(async move { this.approve_inner(request).await })
    }

    async fn approve_inner(&self, request: ExternalApproval) -> Decision {
        if self.state(|s| s.closed || s.denied.is_some() || s.no_approver) {
            return Decision::Deny;
        }
        let Some(info) = self
            .connections()
            .into_iter()
            .find(|c| c.id == request.connection)
        else {
            self.refuse(&request.sql, request.class);
            return Decision::Deny;
        };
        self.record(&info);
        if info.read_only {
            self.refuse(&request.sql, request.class);
            return Decision::Deny;
        }
        if !request.params.is_empty() {
            self.refuse(&request.sql, request.class);
            return Decision::Deny;
        }
        let Some(approver) = self.inner.approver.clone() else {
            self.plan_refused(&request.sql, request.class);
            self.state(|s| s.no_approver = true);
            return Decision::Deny;
        };
        let second = request.stage == ApprovalStage::ProdSecond;
        let index = self.state(|s| {
            if second {
                s.confirms.saturating_sub(1)
            } else {
                s.confirms += 1;
                s.confirms - 1
            }
        });
        let asked = ApprovalRequest {
            tool: EXTERNAL_TOOL,
            connection: info,
            sql: request.sql.clone(),
            class: request.class,
            reason: request.reason,
            stage: request.stage,
            index,
            count: index + 1,
        };
        // The lock is not held here.
        let decision = approver.approve(&asked).await;
        if decision != Decision::Approve {
            self.refuse(&request.sql, request.class);
            return Decision::Deny;
        }
        let last = match request.stage {
            ApprovalStage::ProdSecond => true,
            _ => asked.connection.environment != Some(Environment::Prod),
        };
        if last {
            self.ran.with(|s| {
                s.planned.push((request.sql.clone(), request.class, true));
                s.executed.push(ExecutedStatement {
                    sql: request.sql,
                    class: request.class,
                    approved: true,
                    rows: 0,
                    truncated: None,
                    failed: false,
                });
                s.phase = s.phase.max(Phase::Approved);
            });
        }
        Decision::Approve
    }
}

/// The tools of `external` that may be listed: valid name, not a built-in, not a duplicate, an
/// object schema; descriptions clipped.
pub(crate) fn listed(external: &dyn ExternalTools) -> Vec<ExternalTool> {
    let mut out: Vec<ExternalTool> = Vec::new();
    for mut tool in external.tools() {
        let ok = valid_name(&tool.name)
            && !schema::is_builtin(&tool.name)
            && tool.input_schema.get("type").and_then(Value::as_str) == Some("object")
            && !out.iter().any(|t| t.name == tool.name);
        if !ok {
            continue;
        }
        clip(&mut tool.description, MAX_EXTERNAL_DESCRIPTION_BYTES);
        out.push(tool);
    }
    out
}

fn valid_name(name: &str) -> bool {
    (1..=MAX_NAME_BYTES).contains(&name.len())
        && name.contains(EXTERNAL_NAME_SEPARATOR)
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn clip(text: &mut String, max: usize) {
    if text.len() <= max {
        return;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
}

/// Checks arguments against the supported subset of JSON Schema: an object; `required`;
/// top-level `properties.<k>.type` and `enum`; `additionalProperties: false`.
pub(crate) fn check_arguments(schema: &Value, arguments: &Value) -> Result<(), ToolError> {
    let invalid = |text: String| Err(ToolError::InvalidArguments(text));
    let Some(object) = arguments.as_object() else {
        return invalid("the arguments must be a JSON object".to_owned());
    };
    let key = |k: &str| {
        let mut k = k.to_owned();
        clip(&mut k, MAX_KEY_IN_MESSAGE);
        k
    };
    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        for name in required.iter().filter_map(Value::as_str) {
            if !object.contains_key(name) {
                return invalid(format!("missing field `{}`", key(name)));
            }
        }
    }
    let properties = schema.get("properties").and_then(Value::as_object);
    if schema.get("additionalProperties") == Some(&Value::Bool(false)) {
        for name in object.keys() {
            if !properties.is_some_and(|p| p.contains_key(name)) {
                return invalid(format!("unknown field `{}`", key(name)));
            }
        }
    }
    for (name, value) in object {
        let Some(spec) = properties.and_then(|p| p.get(name)) else {
            continue;
        };
        if let Some(types) = spec.get("type")
            && !type_matches(types, value)
        {
            return invalid(format!("field `{}` has the wrong type", key(name)));
        }
        if let Some(allowed) = spec.get("enum").and_then(Value::as_array)
            && !allowed.contains(value)
        {
            return invalid(format!("field `{}` is not an allowed value", key(name)));
        }
    }
    Ok(())
}

/// Whether `value` has one of the JSON types named in `types` (a name or an array of names).
/// Names this check does not know add no constraint.
fn type_matches(types: &Value, value: &Value) -> bool {
    let names: Vec<&str> = match types {
        Value::String(s) => vec![s.as_str()],
        Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
        _ => return true,
    };
    let mut known = false;
    for name in names {
        let hit = match name {
            "null" => value.is_null(),
            "boolean" => value.is_boolean(),
            "integer" => {
                value.is_i64()
                    || value.is_u64()
                    || value
                        .as_f64()
                        .is_some_and(|f| f.is_finite() && f.fract() == 0.0)
            }
            "number" => value.is_number(),
            "string" => value.is_string(),
            "array" => value.is_array(),
            "object" => value.is_object(),
            _ => continue,
        };
        known = true;
        if hit {
            return true;
        }
    }
    !known
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn names() {
        assert!(valid_name("plug__tool"));
        assert!(valid_name("a-b__c_d"));
        assert!(!valid_name("plain"));
        assert!(!valid_name("a__b c"));
        assert!(!valid_name(""));
        assert!(!valid_name(&format!("a__{}", "x".repeat(70))));
    }

    #[test]
    fn clips_on_a_char_boundary() {
        let mut text = "é".repeat(600);
        clip(&mut text, 1024);
        assert!(text.len() <= 1024);
        assert!(text.chars().all(|c| c == 'é'));
    }

    #[test]
    fn schema_subset() {
        let schema = json!({
            "type": "object",
            "required": ["a"],
            "additionalProperties": false,
            "properties": {
                "a": {"type": "string", "enum": ["x", "y"]},
                "n": {"type": ["integer", "null"]},
            },
        });
        assert!(check_arguments(&schema, &json!({"a": "x", "n": null})).is_ok());
        assert!(check_arguments(&schema, &json!({"a": "x", "n": 3})).is_ok());
        assert!(check_arguments(&schema, &json!({})).is_err());
        assert!(check_arguments(&schema, &json!({"a": "z"})).is_err());
        assert!(check_arguments(&schema, &json!({"a": "x", "n": 1.5})).is_err());
        assert!(check_arguments(&schema, &json!({"a": "x", "b": 1})).is_err());
        assert!(check_arguments(&schema, &json!([1])).is_err());
    }
}
