# Exploration sessions

An exploration session is a conversation with a model about one connection. You ask in plain words; the model reads the schema and, when your question needs data, runs queries through the same tools and guards as everything else. The design is in [ADR-0039](../adr/0039-ai-exploration-session-engine.md). This page describes the engine in `sqler-ai-tools` (`sqler_ai_tools::session`); the session panel in the app uses it.

## What you see

Each message you send starts one turn. During a turn the session shows:

- the streamed answer text;
- each tool call (name, redacted arguments, ok or error);
- each SQL statement that ran, with its class, whether it was approved, the row count and whether the result was truncated;
- notices: retrying after a rate limit or network error, a limit reached, a repeated call stopped, output cut, cancelled, provider error.

The state is one of idle, streaming, tool call pending, awaiting approval, done, cancelled or failed.

Schema questions ("which tables hold orders?") are answered with the catalog tools only and fetch no rows. Data questions use `run_query` or `sample_rows` with small limits.

## Approvals

Reads run without asking. Every other statement waits for you: the session shows the request and the model waits. Production connections ask twice. If you deny, nothing runs; the model gets the refusal and is told not to retry. With no approver configured, such statements are refused. Time spent waiting for you does not count toward the turn time limit. See [Tool contract](tool-contract.md).

## Limits

| Limit | Default |
| ----- | ------- |
| Model steps per turn | 10 |
| Tool calls per turn | 25 |
| Tokens per session | 200,000 |
| Time per turn | 5 minutes |
| Identical calls per turn | 2 |

When a limit is reached the turn ends with a notice and the session stays usable (except the token budget, which covers the whole session). A model that repeats the same call is stopped (loop detected). Calls that were not run are listed as "not run". Rate-limit, provider-overloaded and network errors are retried up to twice with a short delay; other provider errors end the turn.

## Cancel

Cancel stops the stream, the pending tool call (the driver is cancelled) or the wait for an approval. The turn ends as cancelled; you can send the next message right away. A cancel while you are still asked to approve runs nothing and the call is listed as "not run". A cancel after that is listed as "cancelled", not "not run": the statement may already have run, and the audit log has one `cancelled` entry that says which statements were sent. Closing the tab drops the session.

## Re-run

Every executed statement in the transcript can be re-run: the exact SQL and its connection go to the normal run flow, with its usual confirmation. Re-running does not call the model.

## Privacy

Redaction, the untrusted-data envelope and the audit log apply to every call ([Safety](safety.md)). The transcript is kept in memory for the life of the tab and is never written to disk; the session holds no key. The audit log records statements and outcomes, never cell values.
