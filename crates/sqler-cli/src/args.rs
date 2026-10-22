// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Command-line argument parsing, with std only.

use std::fmt;
use std::path::PathBuf;

/// The driver used when `--driver` is not given.
const DEFAULT_DRIVER: &str = "sqlite";

/// The usage text, printed for `--help` and after a usage error.
pub(crate) const USAGE: &str = "\
Usage:
  sqler-cli run [--driver <ID>] <TARGET> <SQL>
  sqler-cli export [--driver <ID>] --format <FMT> --output <PATH|-> [--table <NAME>]
                   [--schema <NAME>] [--allow-write] <TARGET> <SQL>
  sqler-cli mcp [--stdio | --http[=PORT]] [--token-file <PATH>] ([--driver <ID>] <TARGET>)...
  sqler-cli --help | -h
  sqler-cli --version | -V

Commands:
  run         Run one SQL statement and print the result as TSV
  export      Stream the result of one read statement into a file
  mcp         Serve the databases to an AI agent over MCP (stdio, or HTTP on 127.0.0.1)

Options:
  --driver <ID>   Driver id: sqlite, postgres, mysql (default: sqlite)
  --format <FMT>  export: csv, json, sql, markdown (parquet, xlsx when built in)
  --output <PATH> export: the file to write, or - for stdout (text formats only)
  --table <NAME>  export: the table name of the sql format (required for it)
  --schema <NAME> export: the schema of the sql format
  --allow-write   export: also run a statement that is not a plain read
  --stdio         mcp: serve on stdin/stdout (default)
  --http[=PORT]   mcp: serve on 127.0.0.1:PORT (default: a free port); needs a bearer token
  --token-file <PATH>  mcp: read the HTTP token from PATH, or create it there (mode 0600; Unix only,
                  a file readable by others is refused; Windows uses the directory ACL)
                  Without it, SQLER_MCP_TOKEN is used, else a token is generated and printed once
  --              End of options: what follows is <TARGET> and <SQL> (for SQL that starts with -)
  -h, --help      Print this help
  -V, --version   Print the version

Examples:
  sqler-cli run :memory: \"select 1 as a\"
  sqler-cli run ./data.db \"select * from users\"
  sqler-cli run --driver postgres postgres://user@localhost/db \"select 1\"
  sqler-cli export --format csv --output users.csv ./data.db \"select * from users\"
  sqler-cli mcp ./data.db
  sqler-cli mcp --http 0 --driver postgres postgres://user@localhost/db
";

/// A parsed command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Command {
    /// Run one statement.
    Run {
        /// Driver id.
        driver: String,
        /// Driver-specific target (file path, `:memory:` or URL).
        target: String,
        /// The SQL text.
        sql: String,
    },
    /// Export one statement's result to a file or stdout.
    Export(ExportArgs),
    /// Serve the databases over MCP.
    Mcp(McpArgs),
    /// Print the usage.
    Help,
    /// Print the version.
    Version,
}

/// Where an export goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Output {
    /// Standard output (`--output -`).
    Stdout,
    /// A file, written through a temporary sibling and renamed on success.
    File(PathBuf),
}

/// The arguments of `export`. `Debug` would show the target, which can hold a password: do not
/// print it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExportArgs {
    /// Driver id.
    pub(crate) driver: String,
    /// Driver-specific target (file path, `:memory:` or URL).
    pub(crate) target: String,
    /// The SQL text.
    pub(crate) sql: String,
    /// The raw format id, resolved later.
    pub(crate) format: String,
    /// The destination.
    pub(crate) output: Output,
    /// The table name of the `sql` format.
    pub(crate) table: Option<String>,
    /// The schema of the `sql` format.
    pub(crate) schema: Option<String>,
    /// Whether a statement that is not a plain read may run.
    pub(crate) allow_write: bool,
}

/// How `mcp` talks to its client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum McpTransport {
    /// Newline-delimited JSON-RPC on stdin/stdout.
    Stdio,
    /// Streamable HTTP on `127.0.0.1:port` (`0` picks a free port).
    Http {
        /// The port.
        port: u16,
    },
}

/// The arguments of `mcp`. `Debug` hides the targets, which can hold a password.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct McpArgs {
    /// The transport.
    pub(crate) transport: McpTransport,
    /// Where the HTTP token is read from or created.
    pub(crate) token_file: Option<PathBuf>,
    /// `(driver id, target)` per database, in order; they become `db1..dbN`.
    pub(crate) targets: Vec<(String, String)>,
}

impl fmt::Debug for McpArgs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpArgs")
            .field("transport", &self.transport)
            .field("token_file", &self.token_file)
            .field("targets", &self.targets.len())
            .finish()
    }
}

/// A malformed command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UsageError(pub(crate) String);

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Parses the arguments (without `argv[0]`).
pub(crate) fn parse(args: impl IntoIterator<Item = String>) -> Result<Command, UsageError> {
    let mut args = args.into_iter();
    let Some(first) = args.next() else {
        return Err(UsageError("missing command".to_owned()));
    };
    match first.as_str() {
        "--help" | "-h" => Ok(Command::Help),
        "--version" | "-V" => Ok(Command::Version),
        "run" => parse_run(args),
        "export" => parse_export(args),
        "mcp" => parse_mcp(args),
        other if other.starts_with('-') => Err(UsageError("unknown option; see --help".to_owned())),
        _ => Err(UsageError("unknown command; see --help".to_owned())),
    }
}

fn parse_run(mut args: impl Iterator<Item = String>) -> Result<Command, UsageError> {
    let mut driver = DEFAULT_DRIVER.to_owned();
    let mut positional: Vec<String> = Vec::new();
    while let Some(arg) = args.next() {
        if arg == "--" {
            positional.extend(args.by_ref());
        } else if arg == "--driver" {
            driver = args
                .next()
                .ok_or_else(|| UsageError("--driver needs a value".to_owned()))?;
        } else if let Some(value) = arg.strip_prefix("--driver=") {
            value.clone_into(&mut driver);
        } else if arg.starts_with('-') && arg.len() > 1 {
            return Err(UsageError("unknown option; see --help".to_owned()));
        } else {
            positional.push(arg);
        }
    }
    let mut positional = positional.into_iter();
    match (positional.next(), positional.next(), positional.next()) {
        (Some(target), Some(sql), None) => Ok(Command::Run {
            driver,
            target,
            sql,
        }),
        (None, ..) => Err(UsageError("missing <TARGET> and <SQL>".to_owned())),
        (Some(_), None, _) => Err(UsageError("missing <SQL>".to_owned())),
        (Some(_), Some(_), Some(_)) => Err(UsageError("too many arguments".to_owned())),
    }
}

/// Takes the value of `--name VALUE` or `--name=VALUE`; `None` if `arg` is another flag.
fn flag_value(
    arg: &str,
    name: &str,
    args: &mut impl Iterator<Item = String>,
) -> Result<Option<String>, UsageError> {
    if arg == name {
        return args
            .next()
            .map(Some)
            .ok_or_else(|| UsageError(format!("{name} needs a value")));
    }
    Ok(arg
        .strip_prefix(name)
        .and_then(|rest| rest.strip_prefix('='))
        .map(str::to_owned))
}

fn parse_export(mut args: impl Iterator<Item = String>) -> Result<Command, UsageError> {
    let mut driver = DEFAULT_DRIVER.to_owned();
    let mut format = None;
    let mut output = None;
    let mut table = None;
    let mut schema = None;
    let mut allow_write = false;
    let mut positional: Vec<String> = Vec::new();
    while let Some(arg) = args.next() {
        if arg == "--" {
            positional.extend(args.by_ref());
        } else if let Some(v) = flag_value(&arg, "--driver", &mut args)? {
            driver = v;
        } else if let Some(v) = flag_value(&arg, "--format", &mut args)? {
            format = Some(v);
        } else if let Some(v) = flag_value(&arg, "--output", &mut args)? {
            output = Some(v);
        } else if let Some(v) = flag_value(&arg, "--table", &mut args)? {
            table = Some(v);
        } else if let Some(v) = flag_value(&arg, "--schema", &mut args)? {
            schema = Some(v);
        } else if arg == "--allow-write" {
            allow_write = true;
        } else if arg.starts_with('-') && arg.len() > 1 {
            return Err(UsageError("unknown option; see --help".to_owned()));
        } else {
            positional.push(arg);
        }
    }
    let format = format.ok_or_else(|| UsageError("missing --format".to_owned()))?;
    let output = output.ok_or_else(|| UsageError("missing --output".to_owned()))?;
    let output = if output == "-" {
        Output::Stdout
    } else if output.is_empty() {
        return Err(UsageError("--output needs a path".to_owned()));
    } else {
        Output::File(PathBuf::from(output))
    };
    let mut positional = positional.into_iter();
    match (positional.next(), positional.next(), positional.next()) {
        (Some(target), Some(sql), None) => Ok(Command::Export(ExportArgs {
            driver,
            target,
            sql,
            format,
            output,
            table,
            schema,
            allow_write,
        })),
        (None, ..) => Err(UsageError("missing <TARGET> and <SQL>".to_owned())),
        (Some(_), None, _) => Err(UsageError("missing <SQL>".to_owned())),
        (Some(_), Some(_), Some(_)) => Err(UsageError("too many arguments".to_owned())),
    }
}

fn parse_mcp(args: impl Iterator<Item = String>) -> Result<Command, UsageError> {
    let mut args = args.peekable();
    let mut driver = DEFAULT_DRIVER.to_owned();
    let mut transport = None;
    let mut token_file = None;
    let mut targets: Vec<(String, String)> = Vec::new();
    while let Some(arg) = args.next() {
        let new = if arg == "--" {
            for target in args.by_ref() {
                targets.push((driver.clone(), target));
            }
            continue;
        } else if arg == "--stdio" {
            McpTransport::Stdio
        } else if arg == "--http" {
            let port = args.next_if(|next| next.parse::<u16>().is_ok());
            McpTransport::Http {
                port: port.and_then(|p| p.parse().ok()).unwrap_or(0),
            }
        } else if let Some(port) = arg.strip_prefix("--http=") {
            let port = port
                .parse()
                .map_err(|_| UsageError("--http needs a port from 0 to 65535".to_owned()))?;
            McpTransport::Http { port }
        } else if let Some(v) = flag_value(&arg, "--driver", &mut args)? {
            driver = v;
            continue;
        } else if let Some(v) = flag_value(&arg, "--token-file", &mut args)? {
            if v.is_empty() {
                return Err(UsageError("--token-file needs a path".to_owned()));
            }
            if token_file.replace(PathBuf::from(v)).is_some() {
                return Err(UsageError("--token-file given twice".to_owned()));
            }
            continue;
        } else if arg.starts_with('-') && arg.len() > 1 {
            return Err(UsageError("unknown option; see --help".to_owned()));
        } else {
            targets.push((driver.clone(), arg));
            continue;
        };
        if transport.replace(new).is_some() {
            return Err(UsageError(
                "give --stdio or --http once, not both".to_owned(),
            ));
        }
    }
    let transport = transport.unwrap_or(McpTransport::Stdio);
    if targets.is_empty() {
        return Err(UsageError("missing <TARGET>".to_owned()));
    }
    if token_file.is_some() && transport == McpTransport::Stdio {
        return Err(UsageError("--token-file needs --http".to_owned()));
    }
    Ok(Command::Mcp(McpArgs {
        transport,
        token_file,
        targets,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> Result<Command, UsageError> {
        parse(args.iter().map(|s| (*s).to_owned()))
    }

    fn run(driver: &str, target: &str, sql: &str) -> Command {
        Command::Run {
            driver: driver.to_owned(),
            target: target.to_owned(),
            sql: sql.to_owned(),
        }
    }

    #[test]
    fn run_default_driver() {
        assert_eq!(
            p(&["run", ":memory:", "select 1"]),
            Ok(run("sqlite", ":memory:", "select 1"))
        );
    }

    #[test]
    fn run_with_driver() {
        assert_eq!(
            p(&["run", "--driver", "pg", "t", "s"]),
            Ok(run("pg", "t", "s"))
        );
        assert_eq!(
            p(&["run", "t", "s", "--driver=pg"]),
            Ok(run("pg", "t", "s"))
        );
    }

    #[test]
    fn run_postgres_url() {
        assert_eq!(
            p(&[
                "run",
                "--driver",
                "postgres",
                "postgres://user@localhost/db",
                "select 1"
            ]),
            Ok(run("postgres", "postgres://user@localhost/db", "select 1"))
        );
        assert!(USAGE.contains("sqler-cli run --driver postgres postgres://user@localhost/db"));
    }

    #[test]
    fn help_and_version() {
        assert_eq!(p(&["--help"]), Ok(Command::Help));
        assert_eq!(p(&["-h"]), Ok(Command::Help));
        assert_eq!(p(&["--version"]), Ok(Command::Version));
        assert_eq!(p(&["-V"]), Ok(Command::Version));
    }

    #[test]
    fn errors() {
        assert!(p(&[]).is_err());
        assert!(p(&["nope"]).is_err());
        assert!(p(&["--nope"]).is_err());
        assert!(p(&["run"]).is_err());
        assert!(p(&["run", "t"]).is_err());
        assert!(p(&["run", "t", "s", "x"]).is_err());
        assert!(p(&["run", "--driver"]).is_err());
        assert!(p(&["run", "--bogus", "t", "s"]).is_err());
    }

    fn export(args: &[&str]) -> ExportArgs {
        match p(args) {
            Ok(Command::Export(a)) => a,
            other => panic!("not an export: {other:?}"),
        }
    }

    #[test]
    fn export_defaults() {
        let a = export(&["export", "--format", "csv", "--output", "o.csv", "t", "s"]);
        assert_eq!(
            a,
            ExportArgs {
                driver: "sqlite".to_owned(),
                target: "t".to_owned(),
                sql: "s".to_owned(),
                format: "csv".to_owned(),
                output: Output::File(PathBuf::from("o.csv")),
                table: None,
                schema: None,
                allow_write: false,
            }
        );
    }

    #[test]
    fn export_all_flags_in_any_order() {
        let a = export(&[
            "export",
            "t",
            "--format=sql",
            "--driver",
            "postgres",
            "--output=-",
            "--table",
            "users",
            "--schema=app",
            "--allow-write",
            "s",
        ]);
        assert_eq!(a.driver, "postgres");
        assert_eq!(a.format, "sql");
        assert_eq!(a.output, Output::Stdout);
        assert_eq!(a.table.as_deref(), Some("users"));
        assert_eq!(a.schema.as_deref(), Some("app"));
        assert!(a.allow_write);
        assert_eq!((a.target.as_str(), a.sql.as_str()), ("t", "s"));
    }

    #[test]
    fn export_dash_is_stdout_only_for_output() {
        let a = export(&["export", "--format", "csv", "--output", "-", "-", "s"]);
        assert_eq!(a.output, Output::Stdout);
        assert_eq!(a.target, "-");
    }

    #[test]
    fn export_errors() {
        let ok = ["--format", "csv", "--output", "o"];
        let with = |extra: &[&str]| {
            let mut v = vec!["export"];
            v.extend_from_slice(extra);
            p(&v)
        };
        assert!(with(&["--output", "o", "t", "s"]).is_err());
        assert!(with(&["--format", "csv", "t", "s"]).is_err());
        assert!(with(&["--format"]).is_err());
        assert!(with(&["--format", "csv", "--output"]).is_err());
        assert!(with(&["--format", "csv", "--output=", "t", "s"]).is_err());
        assert!(with(&[ok[0], ok[1], ok[2], ok[3]]).is_err());
        assert!(with(&[ok[0], ok[1], ok[2], ok[3], "t"]).is_err());
        assert!(with(&[ok[0], ok[1], ok[2], ok[3], "t", "s", "x"]).is_err());
        assert!(with(&[ok[0], ok[1], ok[2], ok[3], "--bogus", "t", "s"]).is_err());
    }

    #[test]
    fn double_dash_ends_options() {
        let a = export(&[
            "export",
            "--format",
            "csv",
            "--output",
            "o",
            "--",
            "t",
            "-- note\nselect 1",
        ]);
        assert_eq!(
            (a.target.as_str(), a.sql.as_str()),
            ("t", "-- note\nselect 1")
        );
        // Flags after `--` are positional, not options.
        assert!(
            p(&[
                "export",
                "--format",
                "csv",
                "--output",
                "o",
                "--",
                "t",
                "s",
                "--allow-write"
            ])
            .is_err()
        );
        assert_eq!(
            p(&["run", "--driver", "pg", "--", "t", "--select"]),
            Ok(run("pg", "t", "--select"))
        );
    }

    #[test]
    fn usage_names_export() {
        assert!(USAGE.contains("sqler-cli export"));
        assert!(USAGE.contains("--allow-write"));
    }

    fn mcp(args: &[&str]) -> McpArgs {
        let mut v = vec!["mcp"];
        v.extend_from_slice(args);
        match p(&v) {
            Ok(Command::Mcp(a)) => a,
            other => panic!("not mcp: {other:?}"),
        }
    }

    fn t(driver: &str, target: &str) -> (String, String) {
        (driver.to_owned(), target.to_owned())
    }

    #[test]
    fn mcp_defaults_to_stdio_and_sqlite() {
        let a = mcp(&["./a.db"]);
        assert_eq!(a.transport, McpTransport::Stdio);
        assert_eq!(a.targets, vec![t("sqlite", "./a.db")]);
        assert_eq!(a.token_file, None);
    }

    #[test]
    fn mcp_http_port_forms() {
        assert_eq!(
            mcp(&["--http", "x.db"]).transport,
            McpTransport::Http { port: 0 }
        );
        assert_eq!(
            mcp(&["--http", "8080", "x.db"]).transport,
            McpTransport::Http { port: 8080 }
        );
        assert_eq!(
            mcp(&["--http=9", "x.db"]).transport,
            McpTransport::Http { port: 9 }
        );
        assert!(p(&["mcp", "--http=99999", "x.db"]).is_err());
        assert!(p(&["mcp", "--http=", "x.db"]).is_err());
    }

    #[test]
    fn mcp_driver_applies_to_later_targets() {
        let a = mcp(&[
            "a.db",
            "--driver",
            "postgres",
            "u1",
            "u2",
            "--driver=sqlite",
            "b.db",
        ]);
        assert_eq!(
            a.targets,
            vec![
                t("sqlite", "a.db"),
                t("postgres", "u1"),
                t("postgres", "u2"),
                t("sqlite", "b.db"),
            ]
        );
    }

    #[test]
    fn mcp_token_file() {
        let a = mcp(&["--http", "--token-file", "tok", "x.db"]);
        assert_eq!(a.token_file, Some(PathBuf::from("tok")));
        assert!(p(&["mcp", "--token-file", "tok", "x.db"]).is_err());
        assert!(p(&["mcp", "--http", "--token-file", "a", "--token-file=b", "x"]).is_err());
    }

    #[test]
    fn mcp_errors() {
        assert!(p(&["mcp"]).is_err());
        assert!(p(&["mcp", "--http"]).is_err());
        assert!(p(&["mcp", "--stdio", "--http", "x.db"]).is_err());
        assert!(p(&["mcp", "--stdio", "--stdio", "x.db"]).is_err());
        assert!(p(&["mcp", "--bogus", "x.db"]).is_err());
        assert!(p(&["mcp", "--driver"]).is_err());
    }

    #[test]
    fn mcp_debug_hides_targets() {
        let a = mcp(&["--driver", "postgres", "postgres://u:s3cret@h/db"]);
        assert!(!format!("{a:?}").contains("s3cret"));
    }
}
