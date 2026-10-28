// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The assist engine: natural language to SQL, explain and fix, grounded in the schema.
//!
//! [`Assist`] is headless. It grounds every request through the [`ToolSet`] (`search_schema`,
//! then `describe_table` for the best matches, `explain_query` without `ANALYZE` for a plan),
//! so each byte of database text reaches the prompt as an enveloped, redacted and audited tool
//! result. It returns text only and never runs generated SQL. Prompts are in `prompts/*.md`
//! and carry [`PROMPT_VERSION`].
//!
//! # Budgets
//!
//! The context window is `AssistOptions::context_tokens`, else the provider's
//! `max_context_tokens`, else 8192 tokens. `budget = (context - max_output_tokens) * 3` bytes.
//! Grounding gets `budget / 2` (at least 2 KiB, at most 48 KiB), history `budget / 5`.
//!
//! # Cancellation
//!
//! Dropping the future drops the provider stream and any tool call in flight.
//! `AssistOptions::timeout` ends a call with [`AssistError::TimedOut`].

mod conversation;
mod diff;
pub(crate) mod grounding;
mod parse;
mod prompts;
mod search;

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use sqler_ai_api::{
    AiError, AiProvider, ChatRequest, Message, ResponseFormat, ToolCall, ToolDefinition, collect,
};
use sqler_config::{contains_redaction, redact_credentials};
use sqler_sql::{Dialect, StatementClass};
use thiserror::Error;

pub use conversation::{Conversation, Turn, TurnKind};
pub use diff::unified_diff;
pub use parse::{ParsedAnswer, extract_sql, parse_sql_answer};
pub use prompts::PROMPT_VERSION;
pub(crate) use search::search_data;
pub use search::{AssistSearch, DataSearch, GeneratedSql};

use prompts::Task;

use crate::guard;
use crate::redact::RedactScope;
use crate::schema::names;
use crate::source::ConnectionInfo;
use crate::toolset::ToolSet;
use crate::untrusted::wrap_untrusted;

/// The context window assumed when neither the options nor the provider say.
const DEFAULT_CONTEXT_TOKENS: u32 = 8192;
/// Bytes per token used for every budget.
const BYTES_PER_TOKEN: usize = 3;
/// Room kept free in the budget for the model's framing.
const HEADROOM: usize = 2 * 1024;
/// The longest driver error text sent to the model.
const MAX_ERROR_BYTES: usize = 4 * 1024;
/// The longest answer text kept in a [`Turn`].
const MAX_ANSWER_BYTES: usize = 8 * 1024;
/// The grounding budget bounds.
const MIN_GROUNDING: usize = 2 * 1024;
const MAX_GROUNDING: usize = 48 * 1024;

/// Why an assist call failed.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum AssistError {
    /// The host lends no connection with this id or name.
    #[error("unknown connection: {0}")]
    UnknownConnection(String),
    /// The question, SQL or error is empty.
    #[error("invalid input: {0}")]
    InvalidInput(String),
    /// The request does not fit the model's context window.
    #[error("too large: {0}")]
    TooLarge(String),
    /// The provider failed.
    #[error("provider error: {0}")]
    Provider(AiError),
    /// The answer holds no SQL.
    #[error("the answer holds no SQL")]
    NoSql,
    /// The model asked for a tool this engine does not offer, or for another connection.
    #[error("the model asked for an unexpected tool call: {0}")]
    UnexpectedTool(String),
    /// The model kept asking for tools after the last request.
    #[error("the model kept asking for tools")]
    ToolLoop,
    /// The call took longer than `AssistOptions::timeout`.
    #[error("the assist call timed out")]
    TimedOut,
}

impl From<AiError> for AssistError {
    fn from(error: AiError) -> Self {
        Self::Provider(error)
    }
}

/// Limits and budgets of an [`Assist`] engine.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct AssistOptions {
    /// The model's context window in tokens; `None` asks the provider, then assumes 8192.
    pub context_tokens: Option<u32>,
    /// The most tables described in one request. Default 8.
    pub max_tables: usize,
    /// The most tokens the model may answer with. Default 1024.
    pub max_output_tokens: u32,
    /// The longest a call may take. Default 120 seconds.
    pub timeout: Duration,
    /// The most rounds of model-requested tool calls. Default 3.
    pub max_tool_rounds: u8,
    /// The most turns of a [`Conversation`] that are kept and sent. Default 6.
    pub max_turns: usize,
}

impl Default for AssistOptions {
    fn default() -> Self {
        Self {
            context_tokens: None,
            max_tables: 8,
            max_output_tokens: 1024,
            timeout: Duration::from_secs(120),
            max_tool_rounds: 3,
            max_turns: 6,
        }
    }
}

impl AssistOptions {
    /// Sets the context window in tokens.
    #[must_use]
    pub fn with_context_tokens(mut self, tokens: u32) -> Self {
        self.context_tokens = Some(tokens);
        self
    }

    /// Sets the most tables described per request.
    #[must_use]
    pub fn with_max_tables(mut self, max: usize) -> Self {
        self.max_tables = max;
        self
    }

    /// Sets the most tokens the model may answer with.
    #[must_use]
    pub fn with_max_output_tokens(mut self, max: u32) -> Self {
        self.max_output_tokens = max;
        self
    }

    /// Sets the per-call timeout.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Sets the most rounds of model-requested tool calls.
    #[must_use]
    pub fn with_max_tool_rounds(mut self, rounds: u8) -> Self {
        self.max_tool_rounds = rounds;
        self
    }

    /// Sets the most turns kept in a conversation.
    #[must_use]
    pub fn with_max_turns(mut self, turns: usize) -> Self {
        self.max_turns = turns;
        self
    }
}

/// A table that was described to the model.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct GroundedTable {
    /// The database, if the catalog has one.
    pub database: Option<String>,
    /// The schema, if the catalog has one.
    pub schema: Option<String>,
    /// The table or view name.
    pub name: String,
    /// The ranking score: 3 per name hit, 2 per column hit, 2 more per exact match.
    pub score: u32,
}

impl GroundedTable {
    pub(crate) fn new(
        database: Option<String>,
        schema: Option<String>,
        name: String,
        score: u32,
    ) -> Self {
        Self {
            database,
            schema,
            name,
            score,
        }
    }
}

/// The schema context found for a request.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Grounding {
    /// The tables described, best first.
    pub tables: Vec<GroundedTable>,
    /// The tool-result envelopes, joined by a newline. The only database text in the prompt.
    pub text: String,
    /// The length of `text` in bytes.
    pub bytes: usize,
    /// Whether a table was left out because of the budget.
    pub truncated: bool,
    /// The search terms taken from the request.
    pub terms: Vec<String>,
}

impl Grounding {
    pub(crate) fn empty(terms: Vec<String>) -> Self {
        Self {
            tables: Vec::new(),
            text: String::new(),
            bytes: 0,
            truncated: false,
            terms,
        }
    }
}

/// The SQL an NL-to-SQL request produced. Not run.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SqlAnswer {
    /// The statement.
    pub sql: String,
    /// The model's short reasoning.
    pub rationale: String,
    /// The tables the answer was grounded in.
    pub tables: Vec<GroundedTable>,
    /// Whether the statement may be applied: it holds no credential placeholder.
    pub applicable: bool,
    /// The version of the prompt templates used.
    pub prompt_version: u32,
}

/// The explanation of a statement.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Explanation {
    /// The explanation text.
    pub text: String,
    /// Whether a query plan (never `ANALYZE`) was given to the model.
    pub plan_included: bool,
    /// The version of the prompt templates used.
    pub prompt_version: u32,
}

/// A proposed fix. Not run.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct FixProposal {
    /// The SQL as the caller gave it.
    pub original: String,
    /// The fixed SQL.
    pub sql: String,
    /// A unified diff of `original` against `sql`; empty if unchanged.
    pub diff: String,
    /// The model's short reasoning.
    pub rationale: String,
    /// Whether the fix equals the original.
    pub unchanged: bool,
    /// Whether the fix may be applied: changed, and without a credential placeholder.
    pub applicable: bool,
    /// The version of the prompt templates used.
    pub prompt_version: u32,
}

/// The byte budgets of one call.
#[derive(Debug, Clone, Copy)]
struct Budgets {
    total: usize,
    grounding: usize,
    history: usize,
}

/// Splits `total` bytes into the grounding and history budgets of a request whose parts that
/// cannot be cut take `fixed` bytes.
///
/// What is left after the headroom and `fixed` is the room. Grounding takes half of it, within
/// its bounds and never more than the room; history takes the rest, up to a fifth of `total`.
/// While `fixed + HEADROOM <= total`, `fixed + grounding + history + HEADROOM <= total`.
fn split(total: usize, fixed: usize) -> Budgets {
    let room = total.saturating_sub(HEADROOM).saturating_sub(fixed);
    let grounding = (room / 2).clamp(MIN_GROUNDING.min(room), MAX_GROUNDING);
    let history = (room - grounding).min(total / 5);
    Budgets {
        total,
        grounding,
        history,
    }
}

/// The assist engine over a tool set, a provider and a model id.
///
/// Build the [`ToolSet`] without a [`DataSearch`]: an [`AssistSearch`] holds the engine, and a
/// tool set that held the search host would hold itself.
pub struct Assist {
    tools: ToolSet,
    provider: Arc<dyn AiProvider>,
    model: String,
    options: AssistOptions,
}

impl std::fmt::Debug for Assist {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Assist")
            .field("provider", &self.provider.id())
            .field("model", &self.model)
            .field("options", &self.options)
            .finish_non_exhaustive()
    }
}

fn cap_bytes(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn tokens_to_bytes(tokens: u32) -> usize {
    usize::try_from(tokens)
        .unwrap_or(usize::MAX)
        .saturating_mul(BYTES_PER_TOKEN)
}

fn fenced(sql: &str) -> String {
    format!("```sql\n{sql}\n```")
}

fn history_user(kind: TurnKind, request: &str) -> String {
    match kind {
        TurnKind::Explain => format!("Explain this SQL:\n{}", fenced(request)),
        TurnKind::Fix => format!("Fix this SQL:\n{}", fenced(request)),
        _ => format!("Question: {request}"),
    }
}

impl Assist {
    /// An engine with default options.
    #[must_use]
    pub fn new(tools: ToolSet, provider: Arc<dyn AiProvider>, model: impl Into<String>) -> Self {
        Self {
            tools,
            provider,
            model: model.into(),
            options: AssistOptions::default(),
        }
    }

    /// Replaces the options.
    #[must_use]
    pub fn with_options(mut self, options: AssistOptions) -> Self {
        self.options = options;
        self
    }

    /// The bytes the model has for input and output together, less its output cap.
    fn total_bytes(&self) -> usize {
        let context = self
            .options
            .context_tokens
            .or(self.provider.capabilities().max_context_tokens)
            .unwrap_or(DEFAULT_CONTEXT_TOKENS);
        tokens_to_bytes(context.saturating_sub(self.options.max_output_tokens))
    }

    /// The budgets for a request whose parts that cannot be cut take `fixed` bytes.
    fn budgets_with(&self, fixed: usize) -> Budgets {
        split(self.total_bytes(), fixed)
    }

    /// The budgets of a request, or `TooLarge` if the parts that cannot be cut (the system
    /// prompt and `live`) do not fit.
    fn budgets_for(
        &self,
        info: &ConnectionInfo,
        task: Task,
        live: &str,
    ) -> Result<Budgets, AssistError> {
        let fixed = Self::system_for(info, task, true).len() + live.len();
        let room = self.total_bytes().saturating_sub(HEADROOM);
        if fixed > room {
            return Err(AssistError::TooLarge(format!(
                "the request needs {fixed} bytes and the model has room for {room}"
            )));
        }
        Ok(self.budgets_with(fixed))
    }

    fn info(&self, connection: &str) -> Result<ConnectionInfo, AssistError> {
        self.tools
            .connection_info(connection)
            .map_err(|_| AssistError::UnknownConnection(connection.to_owned()))
    }

    async fn timed<T>(
        &self,
        work: impl std::future::Future<Output = Result<T, AssistError>>,
    ) -> Result<T, AssistError> {
        tokio::time::timeout(self.options.timeout, work)
            .await
            .unwrap_or(Err(AssistError::TimedOut))
    }

    /// Finds the tables `text` is about and describes them within the grounding budget.
    ///
    /// # Errors
    /// [`AssistError::UnknownConnection`] or [`AssistError::InvalidInput`] (empty `text`).
    pub async fn ground(&self, connection: &str, text: &str) -> Result<Grounding, AssistError> {
        let info = self.info(connection)?;
        if text.trim().is_empty() {
            return Err(AssistError::InvalidInput("the text is empty".to_owned()));
        }
        let budgets = self.budgets_with(text.len());
        self.timed(grounding::ground(
            &self.tools,
            &info,
            text,
            self.options.max_tables,
            budgets.grounding,
        ))
        .await
    }

    /// Writes SQL for `question`, grounded in the schema of `connection`. The SQL is not run.
    ///
    /// # Errors
    /// See [`AssistError`].
    pub async fn nl_to_sql(
        &self,
        connection: &str,
        question: &str,
        conv: &mut Conversation,
    ) -> Result<SqlAnswer, AssistError> {
        self.nl_to_sql_with(Task::NlToSql, connection, question, conv)
            .await
    }

    pub(crate) async fn nl_to_sql_with(
        &self,
        task: Task,
        connection: &str,
        question: &str,
        conv: &mut Conversation,
    ) -> Result<SqlAnswer, AssistError> {
        let info = self.info(connection)?;
        if question.trim().is_empty() {
            return Err(AssistError::InvalidInput(
                "the question is empty".to_owned(),
            ));
        }
        let question = redact_credentials(question.trim()).into_owned();
        conv.bind(&info.id);
        let live = history_user(TurnKind::NlToSql, &question);
        let budgets = self.budgets_for(&info, task, &live)?;
        let (grounding, text) = self
            .timed(async {
                let grounding = grounding::ground(
                    &self.tools,
                    &info,
                    &question,
                    self.options.max_tables,
                    budgets.grounding,
                )
                .await?;
                let text = self
                    .converse(&info, task, &grounding, &live, conv, &budgets)
                    .await?;
                Ok((grounding, text))
            })
            .await?;
        let parsed = parse_sql_answer(&text)?;
        self.remember(conv, TurnKind::NlToSql, question, &text);
        let applicable = !contains_redaction(&parsed.sql);
        Ok(SqlAnswer {
            sql: parsed.sql,
            rationale: parsed.rationale,
            tables: grounding.tables,
            applicable,
            prompt_version: PROMPT_VERSION,
        })
    }

    /// Explains `sql`. With `with_plan` and a statement that is a plain read, the plan from
    /// `explain_query` (never `ANALYZE`) is given to the model too.
    ///
    /// # Errors
    /// See [`AssistError`].
    pub async fn explain(
        &self,
        connection: &str,
        sql: &str,
        with_plan: bool,
        conv: &mut Conversation,
    ) -> Result<Explanation, AssistError> {
        let info = self.info(connection)?;
        if sql.trim().is_empty() {
            return Err(AssistError::InvalidInput("the SQL is empty".to_owned()));
        }
        let sql = redact_credentials(sql.trim()).into_owned();
        conv.bind(&info.id);
        let base = history_user(TurnKind::Explain, &sql);
        let budgets = self.budgets_for(&info, Task::Explain, &base)?;
        let (plan_included, text) = self
            .timed(async {
                let grounding = grounding::ground(
                    &self.tools,
                    &info,
                    &sql,
                    self.options.max_tables,
                    budgets.grounding,
                )
                .await?;
                let plan = if with_plan {
                    self.plan_of(&info, &sql, &budgets).await
                } else {
                    None
                };
                let live = match &plan {
                    Some(plan) => format!("{base}\n\nQuery plan (tool result):\n{plan}"),
                    None => base.clone(),
                };
                let text = self
                    .converse(&info, Task::Explain, &grounding, &live, conv, &budgets)
                    .await?;
                Ok((plan.is_some(), text))
            })
            .await?;
        let text = text.trim().to_owned();
        if text.is_empty() {
            return Err(AssistError::Provider(AiError::Protocol(
                "the answer is empty".to_owned(),
            )));
        }
        self.remember(conv, TurnKind::Explain, sql, &text);
        Ok(Explanation {
            text,
            plan_included,
            prompt_version: PROMPT_VERSION,
        })
    }

    /// The enveloped plan of `sql`, if every statement is a plain read and the plan fits.
    async fn plan_of(&self, info: &ConnectionInfo, sql: &str, budgets: &Budgets) -> Option<String> {
        let statements = guard::plan(info, sql, None).ok()?;
        if statements
            .iter()
            .any(|s| s.class != StatementClass::Read || s.requires_approval())
        {
            return None;
        }
        let args = json!({"connection": info.id, "sql": sql, "analyze": false});
        let call = ToolCall::new(
            "assist-explain_query",
            names::EXPLAIN_QUERY,
            args.to_string(),
        );
        let result = self.tools.call(&call).await;
        (!result.is_error && result.content.len() <= budgets.total / 4).then_some(result.content)
    }

    /// Proposes a fix of `sql` that failed with `error`. The fix is not run.
    ///
    /// # Errors
    /// See [`AssistError`].
    pub async fn fix(
        &self,
        connection: &str,
        sql: &str,
        error: &str,
        conv: &mut Conversation,
    ) -> Result<FixProposal, AssistError> {
        let info = self.info(connection)?;
        if sql.trim().is_empty() {
            return Err(AssistError::InvalidInput("the SQL is empty".to_owned()));
        }
        if error.trim().is_empty() {
            return Err(AssistError::InvalidInput("the error is empty".to_owned()));
        }
        let shown_sql = redact_credentials(sql.trim()).into_owned();
        let message = redact_credentials(cap_bytes(error.trim(), MAX_ERROR_BYTES)).into_owned();
        let mut body = json!({"error": {"code": "driver", "message": message}});
        let dialect = info.dialect;
        self.tools
            .redaction()
            .apply(&mut body, RedactScope::new(info.send_rows, dialect));
        let envelope = wrap_untrusted(&body);
        conv.bind(&info.id);
        let base = history_user(TurnKind::Fix, &shown_sql);
        let live = format!("{base}\n\nError (tool result):\n{envelope}");
        let budgets = self.budgets_for(&info, Task::Fix, &live)?;
        let text = self
            .timed(async {
                let ground_on = format!("{shown_sql} {message}");
                let grounding = grounding::ground(
                    &self.tools,
                    &info,
                    &ground_on,
                    self.options.max_tables,
                    budgets.grounding,
                )
                .await?;
                let text = self
                    .converse(&info, Task::Fix, &grounding, &live, conv, &budgets)
                    .await?;
                Ok(text)
            })
            .await?;
        let parsed = parse_sql_answer(&text)?;
        let diff = unified_diff(sql.trim(), &parsed.sql, 3);
        let unchanged = diff.is_empty();
        let applicable = !unchanged && !contains_redaction(&parsed.sql);
        self.remember(conv, TurnKind::Fix, shown_sql, &text);
        Ok(FixProposal {
            original: sql.to_owned(),
            sql: parsed.sql,
            diff,
            rationale: parsed.rationale,
            unchanged,
            applicable,
            prompt_version: PROMPT_VERSION,
        })
    }

    fn remember(&self, conv: &mut Conversation, kind: TurnKind, request: String, answer: &str) {
        let answer = cap_bytes(answer.trim(), MAX_ANSWER_BYTES);
        conv.push(Turn::new(kind, request, answer), self.options.max_turns);
    }

    fn system_for(info: &ConnectionInfo, task: Task, json: bool) -> String {
        let dialect = info.dialect.map(Dialect::as_str);
        prompts::system(task, dialect, info.driver.as_str(), json)
    }

    /// The history as messages: the newest turns that fit.
    fn history(&self, conv: &Conversation, budgets: &Budgets) -> Vec<Message> {
        let mut size = 0usize;
        let mut kept = Vec::new();
        for turn in conv.turns().iter().rev().take(self.options.max_turns) {
            let user = history_user(turn.kind, &turn.request);
            size = size.saturating_add(user.len() + turn.answer.len());
            if size > budgets.history {
                break;
            }
            kept.push((user, turn.answer.clone()));
        }
        kept.into_iter()
            .rev()
            .flat_map(|(user, answer)| [Message::user(user), Message::assistant(answer)])
            .collect()
    }

    fn offered_tools(&self) -> Vec<ToolDefinition> {
        self.tools
            .definitions()
            .into_iter()
            .filter(|d| d.name == names::SEARCH_SCHEMA || d.name == names::DESCRIBE_TABLE)
            .collect()
    }

    /// Checks a model tool call before anything runs.
    fn check_call(info: &ConnectionInfo, call: &ToolCall) -> Result<(), AssistError> {
        if call.name != names::SEARCH_SCHEMA && call.name != names::DESCRIBE_TABLE {
            return Err(AssistError::UnexpectedTool(call.name.clone()));
        }
        if let Ok(args) = call.parse_arguments()
            && let Some(target) = args["connection"].as_str()
            && target != info.id
            && target != info.name
        {
            return Err(AssistError::UnexpectedTool(format!(
                "{} on another connection",
                call.name
            )));
        }
        Ok(())
    }

    /// Sends the request, runs the tool rounds and returns the final answer text.
    async fn converse(
        &self,
        info: &ConnectionInfo,
        task: Task,
        grounding: &Grounding,
        live: &str,
        conv: &Conversation,
        budgets: &Budgets,
    ) -> Result<String, AssistError> {
        let caps = self.provider.capabilities();
        let user = if grounding.text.is_empty() {
            live.to_owned()
        } else {
            format!(
                "Schema context (tool results):\n{}\n\n{live}",
                grounding.text
            )
        };
        let mut messages = self.history(conv, budgets);
        messages.push(Message::user(user));
        let mut rounds = 0u8;
        let mut tool_bytes = 0usize;
        loop {
            let offer = caps.tools
                && rounds < self.options.max_tool_rounds
                && grounding.bytes + tool_bytes < budgets.grounding;
            let json = !offer && caps.json_mode && task != Task::Explain;
            let mut request = ChatRequest::new(self.model.clone(), messages.clone())
                .with_system(Self::system_for(info, task, json))
                .with_max_output_tokens(self.options.max_output_tokens);
            if offer {
                request = request.with_tools(self.offered_tools());
            }
            if json {
                request = request.with_response_format(ResponseFormat::Json);
            }
            let mut stream = self.provider.stream(request);
            let response = collect(stream.as_mut()).await?;
            drop(stream);
            if response.tool_calls.is_empty() {
                return Ok(response.text);
            }
            if !offer {
                return Err(AssistError::ToolLoop);
            }
            for call in &response.tool_calls {
                Self::check_call(info, call)?;
            }
            let calls = response.tool_calls.clone();
            messages.push(response.into_message());
            for call in &calls {
                let result = self.tools.call(call).await;
                tool_bytes = tool_bytes.saturating_add(result.content.len());
                messages.push(Message::tool_result(result));
            }
            rounds += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KIB: usize = 1024;

    #[test]
    fn split_keeps_the_invariant() {
        for total in [3, 8, 21, 200].map(|k| k * KIB) {
            for fixed in [0, KIB, 5 * KIB] {
                if fixed + HEADROOM > total {
                    continue;
                }
                let b = split(total, fixed);
                assert!(
                    fixed + b.grounding + b.history + HEADROOM <= total,
                    "total {total} fixed {fixed}: {b:?}"
                );
            }
        }
    }

    #[test]
    fn split_8_kib_with_a_5_kib_statement() {
        let b = split(8 * KIB, 5 * KIB);
        assert_eq!(b.grounding, KIB);
        assert_eq!(b.history, 0);
        assert!(5 * KIB + b.grounding + b.history + HEADROOM <= 8 * KIB);
    }

    #[test]
    fn split_keeps_the_old_shape_when_there_is_plenty_of_room() {
        let b = split(200 * KIB, 0);
        assert_eq!(b.grounding, MAX_GROUNDING);
        assert_eq!(b.history, 200 * KIB / 5);
    }
}
