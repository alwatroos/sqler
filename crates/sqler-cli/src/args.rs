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
  sqler-cli --help | -h
  sqler-cli --version | -V

Commands:
  run         Run one SQL statement and print the result as TSV
  export      Stream the result of one read statement into a file

Options:
  --driver <ID>   Driver id: sqlite, postgres, mysql (default: sqlite)
  --format <FMT>  export: csv, json, sql, markdown (parquet, xlsx when built in)
  --output <PATH> export: the file to write, or - for stdout (text formats only)
  --table <NAME>  export: the table name of the sql format (required for it)
  --schema <NAME> export: the schema of the sql format
  --allow-write   export: also run a statement that is not a plain read
  --              End of options: what follows is <TARGET> and <SQL> (for SQL that starts with -)
  -h, --help      Print this help
  -V, --version   Print the version

Examples:
  sqler-cli run :memory: \"select 1 as a\"
  sqler-cli run ./data.db \"select * from users\"
  sqler-cli run --driver postgres postgres://user@localhost/db \"select 1\"
  sqler-cli export --format csv --output users.csv ./data.db \"select * from users\"
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
}
