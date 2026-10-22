// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Integration tests of the `ACP` client against the mock agent (no network, no real agent).

#![allow(clippy::expect_used, clippy::panic)]

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sqler_acp::{
    AcpError, AcpHost, AgentEvent, AgentEvents, AgentHandle, AgentProfile, LaunchOptions,
    McpServerDescriptor, McpToken, PermissionDecision, PermissionOptionKind, PermissionRequest,
    SessionId, SessionUpdate, StopReason, ToolCallStatus,
};
use tokio::sync::Notify;

const MOCK: &str = env!("CARGO_BIN_EXE_sqler-acp-mock-agent");
const STEP: Duration = Duration::from_secs(30);

async fn within<T>(what: &str, fut: impl Future<Output = T>) -> T {
    tokio::time::timeout(STEP, fut)
        .await
        .unwrap_or_else(|_| panic!("timed out: {what}"))
}

#[derive(Clone, Copy)]
enum Mode {
    Allow,
    Reject,
    Hang,
}

struct TestHost {
    mode: Mode,
    seen: Mutex<Vec<PermissionRequest>>,
    asked: Notify,
}

impl TestHost {
    fn new(mode: Mode) -> Arc<Self> {
        Arc::new(Self {
            mode,
            seen: Mutex::new(Vec::new()),
            asked: Notify::new(),
        })
    }
}

impl AcpHost for TestHost {
    fn request_permission(
        &self,
        req: PermissionRequest,
    ) -> Pin<Box<dyn Future<Output = PermissionDecision> + Send>> {
        let wanted = match self.mode {
            Mode::Allow => Some(PermissionOptionKind::AllowOnce),
            Mode::Reject => Some(PermissionOptionKind::RejectOnce),
            Mode::Hang => None,
        };
        let id = wanted.and_then(|kind| {
            req.options
                .iter()
                .find(|o| o.kind == kind)
                .map(|o| o.id.clone())
        });
        self.seen.lock().expect("lock").push(req);
        self.asked.notify_one();
        Box::pin(async move {
            match (wanted, id) {
                (Some(_), Some(id)) => PermissionDecision::Selected(id),
                (Some(_), None) => PermissionDecision::Cancelled,
                (None, _) => std::future::pending().await,
            }
        })
    }
}

fn profile(args: &[&str]) -> AgentProfile {
    AgentProfile {
        id: "mock".to_owned(),
        name: "Mock Agent".to_owned(),
        command: MOCK.to_owned(),
        args: args.iter().map(|a| (*a).to_owned()).collect(),
        env: Vec::new(),
        inherit_env: Vec::new(),
        install_hint: Some("this is a test double".to_owned()),
    }
}

async fn launch(args: &[&str], mode: Mode) -> (AgentHandle, AgentEvents, Arc<TestHost>) {
    launch_with(args, mode, LaunchOptions::default()).await
}

async fn launch_with(
    args: &[&str],
    mode: Mode,
    opts: LaunchOptions,
) -> (AgentHandle, AgentEvents, Arc<TestHost>) {
    let host = TestHost::new(mode);
    let (handle, events) = within(
        "launch",
        AgentHandle::launch(&profile(args), host.clone(), opts),
    )
    .await
    .expect("launch");
    (handle, events, host)
}

fn cwd() -> PathBuf {
    std::env::current_dir().expect("cwd")
}

async fn session(handle: &AgentHandle) -> SessionId {
    within("new_session", handle.new_session(&cwd(), Vec::new()))
        .await
        .expect("new_session")
}

/// Read events until `pick` returns a value.
async fn until<T>(events: &mut AgentEvents, mut pick: impl FnMut(&AgentEvent) -> Option<T>) -> T {
    within("event", async {
        loop {
            let event = events.recv().await.expect("event stream ended");
            if let Some(value) = pick(&event) {
                return value;
            }
        }
    })
    .await
}

async fn next_message(events: &mut AgentEvents) -> String {
    until(events, |e| match e {
        AgentEvent::Update {
            update: SessionUpdate::AgentMessage(text),
            ..
        } => Some(text.clone()),
        _ => None,
    })
    .await
}

#[cfg(unix)]
fn process_exists(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[tokio::test(flavor = "multi_thread")]
async fn handshake_reports_the_agent() {
    let (handle, _events, _host) = launch(&[], Mode::Allow).await;
    let info = handle.info();
    assert_eq!(info.protocol_version, 1);
    assert_eq!(info.name.as_deref(), Some("sqler-acp-mock-agent"));
    assert_eq!(info.version.as_deref(), Some("0.1.0"));
    assert!(info.mcp_http);
    assert!(!info.mcp_sse);
    assert!(handle.pid().is_some());
    let exit = within("close", handle.close()).await;
    assert_eq!(exit.code, Some(0));
}

#[tokio::test(flavor = "multi_thread")]
async fn echo_streams_messages_plan_and_tool_call() {
    let (handle, mut events, _host) = launch(&[], Mode::Allow).await;
    let id = session(&handle).await;
    let stop = within("prompt", handle.prompt(&id, "echo".to_owned()))
        .await
        .expect("prompt");
    assert_eq!(stop, StopReason::EndTurn);

    let mut text = String::new();
    let mut plan = None;
    let mut call = None;
    while call.is_none() {
        match within("event", events.recv()).await.expect("event") {
            AgentEvent::Update { session_id, update } => {
                assert_eq!(session_id, id);
                match update {
                    SessionUpdate::AgentMessage(chunk) => text.push_str(&chunk),
                    SessionUpdate::Plan(entries) => plan = Some(entries),
                    SessionUpdate::ToolCall(info) => call = Some(info),
                    other => panic!("unexpected update {other:?}"),
                }
            }
            other => panic!("unexpected event {other:?}"),
        }
    }
    assert_eq!(text, "Hello world");
    let plan = plan.expect("plan");
    assert_eq!(plan.len(), 2);
    assert_eq!(
        (plan[0].priority.as_str(), plan[0].status.as_str()),
        ("high", "completed")
    );
    let call = call.expect("tool call");
    assert_eq!(call.title.as_deref(), Some("List tables"));
    assert_eq!(call.kind.as_deref(), Some("read"));
    assert_eq!(call.status, Some(ToolCallStatus::InProgress));
    within("close", handle.close()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn permission_approved_and_denied() {
    for (mode, expected) in [
        (Mode::Allow, "selected:allow-once"),
        (Mode::Reject, "selected:reject-once"),
    ] {
        let (handle, mut events, host) = launch(&[], mode).await;
        let id = session(&handle).await;
        let stop = within("prompt", handle.prompt(&id, "permission".to_owned()))
            .await
            .expect("prompt");
        assert_eq!(stop, StopReason::EndTurn);
        assert_eq!(next_message(&mut events).await, expected);
        {
            let seen = host.seen.lock().expect("lock");
            assert_eq!(seen.len(), 1);
            assert_eq!(seen[0].session_id, id);
            assert_eq!(seen[0].tool_call.title.as_deref(), Some("Run DELETE"));
            assert_eq!(seen[0].options.len(), 4);
        }
        within("close", handle.close()).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_answers_a_pending_permission_request() {
    let (handle, mut events, host) = launch(&[], Mode::Hang).await;
    let handle = Arc::new(handle);
    let id = session(&handle).await;
    let prompt = {
        let (handle, id) = (Arc::clone(&handle), id.clone());
        tokio::spawn(async move { handle.prompt(&id, "permission".to_owned()).await })
    };
    within("host asked", host.asked.notified()).await;
    within("cancel", handle.cancel(&id)).await.expect("cancel");
    let stop = within("prompt", prompt)
        .await
        .expect("join")
        .expect("prompt");
    assert_eq!(stop, StopReason::Cancelled);
    assert_eq!(next_message(&mut events).await, "cancelled");
    let handle = Arc::try_unwrap(handle).expect("sole owner");
    within("close", handle.close()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_ends_a_hanging_prompt() {
    let (handle, _events, _host) = launch(&[], Mode::Allow).await;
    let handle = Arc::new(handle);
    let id = session(&handle).await;
    let prompt = {
        let (handle, id) = (Arc::clone(&handle), id.clone());
        tokio::spawn(async move { handle.prompt(&id, "hang".to_owned()).await })
    };
    tokio::time::sleep(Duration::from_millis(200)).await;
    // A second prompt in the same session is refused while the first runs.
    let second = within("second prompt", handle.prompt(&id, "echo".to_owned())).await;
    assert!(
        matches!(second, Err(AcpError::PromptInProgress)),
        "{second:?}"
    );
    within("cancel", handle.cancel(&id)).await.expect("cancel");
    let stop = within("prompt", prompt)
        .await
        .expect("join")
        .expect("prompt");
    assert_eq!(stop, StopReason::Cancelled);
    // The agent is still usable.
    let stop = within("prompt", handle.prompt(&id, "echo".to_owned()))
        .await
        .expect("prompt");
    assert_eq!(stop, StopReason::EndTurn);
    let handle = Arc::try_unwrap(handle).expect("sole owner");
    within("close", handle.close()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn deaf_agent_is_killed_after_the_cancel_grace() {
    let opts = LaunchOptions {
        cancel_grace: Duration::from_millis(300),
        ..LaunchOptions::default()
    };
    let (handle, _events, _host) = launch_with(&[], Mode::Allow, opts).await;
    let handle = Arc::new(handle);
    let id = session(&handle).await;
    let mut exit_watch = handle.exit_watch();
    let pid = handle.pid().expect("pid");
    let prompt = {
        let (handle, id) = (Arc::clone(&handle), id.clone());
        tokio::spawn(async move { handle.prompt(&id, "hang-deaf".to_owned()).await })
    };
    tokio::time::sleep(Duration::from_millis(200)).await;
    within("cancel", handle.cancel(&id)).await.expect("cancel");
    let stop = within("prompt", prompt)
        .await
        .expect("join")
        .expect("prompt");
    assert_eq!(stop, StopReason::Cancelled);
    let exit = within("exit", exit_watch.wait()).await;
    #[cfg(unix)]
    assert_eq!(exit.signal, Some(9), "{exit:?}");
    #[cfg(not(unix))]
    assert_ne!(exit.code, Some(0), "{exit:?}");
    #[cfg(unix)]
    assert!(!process_exists(pid), "the agent process was not reaped");
    #[cfg(not(unix))]
    let _ = pid;
    // Later calls report the exit.
    let later = within("prompt after kill", handle.prompt(&id, "echo".to_owned())).await;
    assert!(
        matches!(later, Err(AcpError::AgentExited { .. })),
        "{later:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn crash_fails_the_prompt_and_reports_the_exit() {
    let (handle, mut events, _host) = launch(&[], Mode::Allow).await;
    let mut exit_watch = handle.exit_watch();
    let id = session(&handle).await;
    let result = within("prompt", handle.prompt(&id, "crash".to_owned())).await;
    let Err(AcpError::AgentExited { profile, exit }) = result else {
        panic!("expected AgentExited, got {result:?}");
    };
    assert_eq!(profile, "Mock Agent");
    assert_eq!(exit.code, Some(3));
    assert!(
        exit.stderr_tail.iter().any(|l| l == "mock: crashing"),
        "{exit:?}"
    );

    let exited = until(&mut events, |e| match e {
        AgentEvent::Exited(info) => Some(info.clone()),
        _ => None,
    })
    .await;
    assert_eq!(exited.code, Some(3));
    assert_eq!(within("exit watch", exit_watch.wait()).await.code, Some(3));
    assert!(within("events end", events.recv()).await.is_none());
    // Further calls fail cleanly.
    assert!(matches!(
        within("new_session", handle.new_session(&cwd(), Vec::new())).await,
        Err(AcpError::AgentExited { .. })
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn noisy_stderr_is_bounded_and_the_session_stays_healthy() {
    let (handle, mut events, _host) = launch(&[], Mode::Allow).await;
    let id = session(&handle).await;
    // Nobody drains the events during the prompt: the reader must neither stall nor grow without bound.
    let reply = within("prompt", handle.prompt(&id, "noisy".to_owned())).await;
    assert!(reply.is_ok(), "{reply:?}");
    assert_eq!(next_message(&mut events).await, "noisy ok");
    // The session still works after the flood.
    let reply = within("prompt", handle.prompt(&id, "echo".to_owned())).await;
    assert!(reply.is_ok(), "{reply:?}");
    let exit = within("close", handle.close()).await;
    assert!(
        exit.stderr_tail.len() <= 64 && exit.stderr_tail.iter().all(|l| l.chars().count() <= 4096),
        "{} lines",
        exit.stderr_tail.len()
    );
    assert!(
        exit.stderr_tail.iter().any(|l| l == "mock: noisy done"),
        "tail lost the last line"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn crash_on_init_fails_the_launch() {
    let host = TestHost::new(Mode::Allow);
    let result = within(
        "launch",
        AgentHandle::launch(
            &profile(&["--crash-on-init"]),
            host,
            LaunchOptions::default(),
        ),
    )
    .await;
    let Err(AcpError::AgentExited { exit, .. }) = result else {
        panic!("expected AgentExited, got {:?}", result.map(|_| ()));
    };
    assert_eq!(exit.code, Some(4));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_agent_sees_the_injected_descriptors() {
    let (handle, mut events, _host) = launch(&[], Mode::Allow).await;
    let servers = vec![
        McpServerDescriptor::Stdio {
            name: "sqler-stdio".to_owned(),
            command: PathBuf::from("/usr/local/bin/sqler"),
            args: vec!["mcp".to_owned(), "--read-only".to_owned()],
            env: vec![("MODE".to_owned(), "ro".to_owned())],
        },
        McpServerDescriptor::Http {
            name: "sqler-http".to_owned(),
            url: "http://127.0.0.1:4711/mcp".to_owned(),
            token: McpToken::new("tok-123".to_owned()),
        },
    ];
    let id = within("new_session", handle.new_session(&cwd(), servers))
        .await
        .expect("new_session");
    within("prompt", handle.prompt(&id, "descriptor".to_owned()))
        .await
        .expect("prompt");
    let json: serde_json::Value =
        serde_json::from_str(&next_message(&mut events).await).expect("json");
    let list = json.as_array().expect("array");
    assert_eq!(list.len(), 2);
    let stdio = &list[0];
    assert_eq!(stdio["name"], "sqler-stdio");
    assert_eq!(stdio["command"], "/usr/local/bin/sqler");
    assert_eq!(stdio["args"], serde_json::json!(["mcp", "--read-only"]));
    assert_eq!(stdio["env"][0]["name"], "MODE");
    let http = &list[1];
    assert_eq!(http["type"], "http");
    assert_eq!(http["url"], "http://127.0.0.1:4711/mcp");
    assert_eq!(http["headers"][0]["name"], "Authorization");
    assert_eq!(http["headers"][0]["value"], "Bearer tok-123");
    within("close", handle.close()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn http_descriptor_needs_an_agent_with_http_mcp() {
    let (handle, _events, _host) = launch(&["--no-mcp-http"], Mode::Allow).await;
    assert!(!handle.info().mcp_http);
    let http = McpServerDescriptor::Http {
        name: "sqler".to_owned(),
        url: "http://127.0.0.1:1/mcp".to_owned(),
        token: McpToken::new("tok-123".to_owned()),
    };
    let err = within("new_session", handle.new_session(&cwd(), vec![http]))
        .await
        .expect_err("must refuse");
    assert!(
        matches!(
            err,
            AcpError::McpTransportUnsupported {
                transport: "http",
                ..
            }
        ),
        "{err:?}"
    );
    assert!(!err.to_string().contains("tok-123"));
    // Stdio is still fine.
    let stdio = McpServerDescriptor::Stdio {
        name: "sqler".to_owned(),
        command: PathBuf::from("/bin/sqler"),
        args: Vec::new(),
        env: Vec::new(),
    };
    within("new_session", handle.new_session(&cwd(), vec![stdio]))
        .await
        .expect("stdio session");
    within("close", handle.close()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_session_arguments_are_refused() {
    let (handle, _events, _host) = launch(&[], Mode::Allow).await;
    let relative = within(
        "new_session",
        handle.new_session(Path::new("relative/dir"), Vec::new()),
    )
    .await;
    assert!(
        matches!(relative, Err(AcpError::InvalidCwd)),
        "{relative:?}"
    );
    let remote = McpServerDescriptor::Http {
        name: "sqler".to_owned(),
        url: "http://example.com/mcp".to_owned(),
        token: McpToken::new("tok-123".to_owned()),
    };
    let err = within("new_session", handle.new_session(&cwd(), vec![remote]))
        .await
        .expect_err("must refuse");
    assert!(matches!(err, AcpError::InvalidDescriptor(_)), "{err:?}");
    assert!(!err.to_string().contains("tok-123"));
    let unknown = SessionId::new("nope");
    let result = within("prompt", handle.prompt(&unknown, "echo".to_owned())).await;
    assert!(
        matches!(result, Err(AcpError::UnknownSession)),
        "{result:?}"
    );
    within("close", handle.close()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_agent_names_the_profile_and_the_hint() {
    let mut missing = profile(&[]);
    missing.command = "sqler-no-such-agent-binary".to_owned();
    missing.name = "Ghost Agent".to_owned();
    missing.install_hint = Some("npm install -g ghost-acp".to_owned());
    let result = within(
        "launch",
        AgentHandle::launch(
            &missing,
            TestHost::new(Mode::Allow),
            LaunchOptions::default(),
        ),
    )
    .await;
    let Err(err) = result else {
        panic!("expected an error")
    };
    assert!(matches!(err, AcpError::AgentNotFound { .. }));
    let text = err.to_string();
    assert!(text.contains("Ghost Agent"), "{text}");
    assert!(text.contains("sqler-no-such-agent-binary"), "{text}");
    assert!(text.contains("npm install -g ghost-acp"), "{text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_agent_gets_no_secret_in_its_environment_or_arguments() {
    assert!(
        std::env::vars().any(|(k, _)| k.starts_with("CARGO_")),
        "the test run is expected to have CARGO_* variables to prove the scrub"
    );
    let (handle, mut events, _host) = launch(&["--unused-flag", "plain-arg"], Mode::Allow).await;
    let http = McpServerDescriptor::Http {
        name: "sqler".to_owned(),
        url: "http://127.0.0.1:4711/mcp".to_owned(),
        token: McpToken::new("tok-secret-9".to_owned()),
    };
    let id = within("new_session", handle.new_session(&cwd(), vec![http]))
        .await
        .expect("new_session");
    within("prompt", handle.prompt(&id, "env".to_owned()))
        .await
        .expect("prompt");
    let raw = next_message(&mut events).await;
    let json: serde_json::Value = serde_json::from_str(&raw).expect("json");
    assert_eq!(
        json["args"],
        serde_json::json!(["--unused-flag", "plain-arg"])
    );
    let env = json["env"].as_object().expect("env object");
    for key in env.keys() {
        assert!(
            !key.starts_with("CARGO_") && !key.starts_with("RUST_"),
            "leaked {key}"
        );
    }
    assert!(
        !raw.contains("tok-secret-9"),
        "the token reached the environment or arguments"
    );
    assert!(env.keys().any(|k| k.eq_ignore_ascii_case("PATH")));
    within("close", handle.close()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn close_and_drop_both_reap_the_process() {
    let (closed, _events_a, _host_a) = launch(&[], Mode::Allow).await;
    let (dropped, _events_b, _host_b) = launch(&[], Mode::Allow).await;
    let mut watch_a = closed.exit_watch();
    let mut watch_b = dropped.exit_watch();
    let pid_a = closed.pid().expect("pid");
    let pid_b = dropped.pid().expect("pid");

    let exit = within("close", closed.close()).await;
    assert_eq!(exit.code, Some(0));
    assert_eq!(within("watch a", watch_a.wait()).await.code, Some(0));

    drop(dropped);
    assert_eq!(within("watch b", watch_b.wait()).await.code, Some(0));
    #[cfg(unix)]
    {
        assert!(!process_exists(pid_a));
        assert!(!process_exists(pid_b));
    }
    #[cfg(not(unix))]
    let _ = (pid_a, pid_b);
}
