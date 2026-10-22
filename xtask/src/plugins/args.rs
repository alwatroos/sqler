// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Argument parsing for `plugins`.

use super::table::PLUGINS;

/// A parsed subcommand.
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    /// Build and stage plugins; no names means every row.
    Build {
        /// Release profile.
        release: bool,
        /// Selected plugin names, deduplicated, order kept.
        names: Vec<String>,
    },
    /// Guest-workspace fmt and clippy.
    Check,
    /// Print the table.
    List,
    /// Remove `target/plugins`.
    Clean,
}

/// A usage error (exit code 2).
#[derive(Debug, PartialEq, Eq)]
pub struct UsageError(pub String);

fn valid_names() -> String {
    PLUGINS
        .iter()
        .map(|r| r.name)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Parse the arguments after `plugins`.
pub fn parse(args: &[String]) -> Result<Command, UsageError> {
    let Some((sub, rest)) = args.split_first() else {
        return Err(UsageError(
            "missing subcommand, one of: build, check, list, clean".to_owned(),
        ));
    };
    match sub.as_str() {
        "build" => {
            let mut release = false;
            let mut names: Vec<String> = Vec::new();
            for arg in rest {
                if arg == "--release" {
                    release = true;
                } else if arg.starts_with('-') {
                    return Err(UsageError(format!("unknown flag `{arg}`")));
                } else if !PLUGINS.iter().any(|r| r.name == arg) {
                    return Err(UsageError(format!(
                        "unknown plugin `{arg}`, valid names: {}",
                        valid_names()
                    )));
                } else if !names.contains(arg) {
                    names.push(arg.clone());
                }
            }
            Ok(Command::Build { release, names })
        }
        "check" | "list" | "clean" => {
            if let Some(extra) = rest.first() {
                return Err(UsageError(format!(
                    "unexpected argument `{extra}` for `{sub}`"
                )));
            }
            Ok(match sub.as_str() {
                "check" => Command::Check,
                "list" => Command::List,
                _ => Command::Clean,
            })
        }
        other => Err(UsageError(format!(
            "unknown subcommand `{other}`, one of: build, check, list, clean"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::{Command, UsageError, parse};

    fn p(args: &[&str]) -> Result<Command, UsageError> {
        parse(&args.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>())
    }

    #[test]
    fn build_forms() {
        assert_eq!(
            p(&["build"]),
            Ok(Command::Build {
                release: false,
                names: vec![]
            })
        );
        assert_eq!(
            p(&["build", "--release"]),
            Ok(Command::Build {
                release: true,
                names: vec![]
            })
        );
        assert_eq!(
            p(&["build", "smoke", "--release", "smoke"]),
            Ok(Command::Build {
                release: true,
                names: vec!["smoke".to_owned()]
            })
        );
    }

    #[test]
    fn other_subcommands() {
        assert_eq!(p(&["check"]), Ok(Command::Check));
        assert_eq!(p(&["list"]), Ok(Command::List));
        assert_eq!(p(&["clean"]), Ok(Command::Clean));
    }

    #[test]
    fn usage_errors() {
        assert!(p(&[]).is_err());
        assert!(p(&["nope"]).is_err());
        assert!(p(&["build", "--debug"]).is_err());
        assert!(p(&["clean", "x"]).is_err());
        let err = p(&["build", "zzz"]).err().map(|e| e.0).unwrap_or_default();
        assert!(err.contains("smoke"), "{err}");
    }
}
