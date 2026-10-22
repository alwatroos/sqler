// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! End-to-end tests of `sqler-cli mcp`, over stdio and HTTP.

#![cfg(feature = "driver-sqlite")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::{Value, json};

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// A scratch directory holding a seeded database and the config home; removed on drop.
struct Env {
    dir: PathBuf,
}

impl Env {
    fn new(tag: &str) -> Self {
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("mcp-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("home")).unwrap();
        let env = Self { dir };
        for sql in [
            "create table t (id integer primary key, name text)",
            "insert into t values (1, 'ann'), (2, 'bob')",
        ] {
            let out = Command::new(env!("CARGO_BIN_EXE_sqler-cli"))
                .args(["run"])
                .arg(env.db())
                .arg(sql)
                .output()
                .unwrap();
            assert!(out.status.success());
        }
        env
    }

    fn db(&self) -> PathBuf {
        self.dir.join("data.db")
    }

    fn command(&self) -> Command {
        let home = self.dir.join("home");
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_sqler-cli"));
        cmd.env("HOME", &home)
            .env("XDG_CONFIG_HOME", &home)
            .env("APPDATA", &home)
            .env_remove("SQLER_MCP_TOKEN")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd
    }

    fn audit(&self) -> String {
        fn find(dir: &Path) -> Option<PathBuf> {
            for entry in std::fs::read_dir(dir).ok()?.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    if let Some(found) = find(&path) {
                        return Some(found);
                    }
                } else if path.file_name().is_some_and(|n| n == "ai-audit.jsonl") {
                    return Some(path);
                }
            }
            None
        }
        find(&self.dir.join("home"))
            .map(|p| std::fs::read_to_string(p).unwrap())
            .unwrap_or_default()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A child process that is killed when dropped.
struct Guard(Child);

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Stdio2 {
    _guard: Guard,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Stdio2 {
    fn start(env: &Env) -> Self {
        let mut child = env.command().arg("mcp").arg(env.db()).spawn().unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self {
            _guard: Guard(child),
            stdin,
            stdout,
            next_id: 1,
        }
    }

    fn send(&mut self, value: &Value) {
        writeln!(self.stdin, "{value}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn call(&mut self, method: &str, params: &Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let mut line = String::new();
        self.stdout.read_line(&mut line).unwrap();
        let reply: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(reply["id"], id);
        reply
    }

    fn initialize(&mut self) {
        let reply = self.call(
            "initialize",
            &json!({"protocolVersion": "2025-06-18", "capabilities": {},
                   "clientInfo": {"name": "t", "version": "0"}}),
        );
        assert!(
            reply["result"]["capabilities"]["tools"].is_object(),
            "{reply}"
        );
        self.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    }

    fn tool(&mut self, name: &str, args: &Value) -> Value {
        self.call("tools/call", &json!({"name": name, "arguments": args}))
    }
}

#[test]
fn stdio_lists_reads_and_refuses_writes() {
    let env = Env::new("stdio");
    let mut mcp = Stdio2::start(&env);
    mcp.initialize();

    let list = mcp.call("tools/list", &json!({}));
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"run_query"), "{names:?}");
    assert!(!names.contains(&"open_in_editor"));

    let conns = mcp.tool("list_connections", &json!({}));
    let text = conns["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("db1"), "{text}");
    assert!(
        !text.contains("data.db"),
        "the target must not leak: {text}"
    );

    let read = mcp.tool(
        "run_query",
        &json!({"connection": "db1", "sql": "select name from t order by id"}),
    );
    assert_eq!(read["result"]["isError"], false, "{read}");
    let text = read["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("ann") && text.contains("bob"), "{text}");

    let write = mcp.tool(
        "run_query",
        &json!({"connection": "db1", "sql": "insert into t values (3, 'cy')"}),
    );
    assert_eq!(write["result"]["isError"], true, "{write}");
    assert!(
        write["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("no_approver"),
        "{write}"
    );

    let rows = Command::new(env!("CARGO_BIN_EXE_sqler-cli"))
        .args(["run"])
        .arg(env.db())
        .arg("select count(*) from t")
        .output()
        .unwrap();
    assert_eq!(String::from_utf8(rows.stdout).unwrap(), "count(*)\n2\n");

    let audit = env.audit();
    assert!(audit.contains("\"surface\":\"mcp\""), "{audit}");
}

#[test]
fn bad_target_fails_without_echoing_it() {
    let env = Env::new("bad");
    let secret = "/nonexistent-dir-s3cret/none.db";
    let out = env.command().args(["mcp", secret]).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!all.contains("s3cret"), "{all}");
    assert!(all.contains("db1"), "{all}");
}

#[test]
fn usage_error_exits_2() {
    let env = Env::new("usage");
    let out = env.command().args(["mcp"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
}

struct Http {
    _guard: Guard,
    port: u16,
}

impl Http {
    fn start(env: &Env, extra: &[&str], token_env: Option<&str>) -> (Self, Vec<String>) {
        let mut cmd = env.command();
        if let Some(token) = token_env {
            cmd.env("SQLER_MCP_TOKEN", token);
        }
        let mut child = cmd
            .args(["mcp", "--http"])
            .args(extra)
            .arg(env.db())
            .spawn()
            .unwrap();
        let mut out = BufReader::new(child.stdout.take().unwrap());
        let mut lines = Vec::new();
        let mut first = String::new();
        out.read_line(&mut first).unwrap();
        let guard = Guard(child);
        let url = first.trim().rsplit(' ').next().unwrap().to_owned();
        let port: u16 = url
            .strip_prefix("http://127.0.0.1:")
            .and_then(|r| r.strip_suffix("/mcp"))
            .unwrap_or_else(|| panic!("bad start block: {first}"))
            .parse()
            .unwrap();
        lines.push(first.trim().to_owned());
        if token_env.is_none() && !extra.contains(&"--token-file") {
            let mut second = String::new();
            out.read_line(&mut second).unwrap();
            lines.push(second.trim().to_owned());
        }
        (
            Self {
                _guard: guard,
                port,
            },
            lines,
        )
    }

    fn post(&self, token: Option<&str>) -> (u16, String) {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": "t", "version": "0"}}})
        .to_string();
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let auth = token
            .map(|t| format!("Authorization: Bearer {t}\r\n"))
            .unwrap_or_default();
        write!(
            stream,
            "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n{auth}Content-Type: application/json\r\n\
             Accept: application/json, text/event-stream\r\nConnection: close\r\n\
             Content-Length: {}\r\n\r\n{body}",
            self.port,
            body.len()
        )
        .unwrap();
        let mut reply = String::new();
        stream.read_to_string(&mut reply).unwrap();
        let status = reply
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap();
        (status, reply)
    }
}

#[test]
fn http_with_env_token() {
    let env = Env::new("http-env");
    let token = "t".repeat(40);
    let (http, lines) = Http::start(&env, &["0"], Some(&token));
    assert_eq!(lines.len(), 1, "an env token is never printed: {lines:?}");
    assert!(!lines[0].contains(&token));
    assert_eq!(http.post(None).0, 401);
    assert_eq!(http.post(Some(&"u".repeat(40))).0, 401);
    assert_eq!(http.post(Some(&token)).0, 200);
}

#[test]
fn http_generated_token_is_printed_once_on_stdout() {
    let env = Env::new("http-gen");
    let (http, lines) = Http::start(&env, &[], None);
    let token = lines[1].strip_prefix("Authorization: Bearer ").unwrap();
    assert_eq!(token.len(), 43);
    assert_eq!(http.post(None).0, 401);
    assert_eq!(http.post(Some(token)).0, 200);
}

#[test]
fn http_token_file_is_created_private_and_never_printed() {
    let env = Env::new("http-file");
    let file = env.dir.join("token");
    let file_arg = file.to_str().unwrap().to_owned();
    let (http, lines) = Http::start(&env, &["--token-file", &file_arg], None);
    assert_eq!(lines.len(), 1, "{lines:?}");
    let token = std::fs::read_to_string(&file).unwrap();
    let token = token.trim();
    assert_eq!(token.len(), 43);
    assert!(!lines[0].contains(token));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    assert_eq!(http.post(Some(token)).0, 200);
}

#[test]
fn token_env_and_token_file_together_is_a_usage_error() {
    let env = Env::new("both");
    let file = env.dir.join("token");
    let out = env
        .command()
        .env("SQLER_MCP_TOKEN", "x".repeat(43))
        .args(["mcp", "--http", "--token-file"])
        .arg(&file)
        .arg(env.db())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(!file.exists());
}

#[cfg(unix)]
#[test]
fn a_group_readable_token_file_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    let env = Env::new("loose");
    let file = env.dir.join("token");
    std::fs::write(&file, "a".repeat(43)).unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
    let out = env
        .command()
        .args(["mcp", "--http", "--token-file"])
        .arg(&file)
        .arg(env.db())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("chmod 600"));
}
