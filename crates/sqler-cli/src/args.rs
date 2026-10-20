// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Command-line argument parsing, with std only.

use std::fmt;

/// The driver used when `--driver` is not given.
const DEFAULT_DRIVER: &str = "sqlite";

/// The usage text, printed for `--help` and after a usage error.
pub(crate) const USAGE: &str = "\
Usage:
  sqler-cli run [--driver <ID>] <TARGET> <SQL>
  sqler-cli --help | -h
  sqler-cli --version | -V

Commands:
  run         Run one SQL statement and print the result as TSV

Options:
  --driver <ID>   Driver id: sqlite, postgres (default: sqlite)
  -h, --help      Print this help
  -V, --version   Print the version

Examples:
  sqler-cli run :memory: \"select 1 as a\"
  sqler-cli run ./data.db \"select * from users\"
  sqler-cli run --driver postgres postgres://user@localhost/db \"select 1\"
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
    /// Print the usage.
    Help,
    /// Print the version.
    Version,
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
        other if other.starts_with('-') => Err(UsageError(format!("unknown option: {other}"))),
        other => Err(UsageError(format!("unknown command: {other}"))),
    }
}

fn parse_run(mut args: impl Iterator<Item = String>) -> Result<Command, UsageError> {
    let mut driver = DEFAULT_DRIVER.to_owned();
    let mut positional: Vec<String> = Vec::new();
    while let Some(arg) = args.next() {
        if arg == "--driver" {
            driver = args
                .next()
                .ok_or_else(|| UsageError("--driver needs a value".to_owned()))?;
        } else if let Some(value) = arg.strip_prefix("--driver=") {
            value.clone_into(&mut driver);
        } else if arg.starts_with('-') && arg.len() > 1 {
            return Err(UsageError(format!("unknown option: {arg}")));
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
}
