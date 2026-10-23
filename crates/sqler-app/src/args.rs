// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Command-line argument parsing, with std only.

use std::fmt;
use std::path::PathBuf;

/// The usage text, printed for `--help` and after a usage error.
pub(crate) const USAGE: &str = "\
Usage:
  sqler [--exit-after-first-frame | --open-sessions] [--plugin-dir <PATH>]...
  sqler --help | -h
  sqler --version | -V

Options:
  --exit-after-first-frame   Exit with code 0 once the first frame is presented
  --open-sessions            Open every saved connection after startup, then idle (idle RSS measurement)
  --plugin-dir <PATH>        Also load a plugin (a folder with sqler.toml) or every plugin below a folder;
                             repeatable; only in builds with plugin support
  -h, --help                 Print this help
  -V, --version              Print the version
";

/// A parsed command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Command {
    /// Start the application.
    Run {
        /// Exit as soon as the first frame has been presented.
        exit_after_first_frame: bool,
        /// Open every saved connection after startup, then idle (idle RSS measurement).
        open_sessions: bool,
        /// Development plugin paths (`--plugin-dir`), in the order given. The only source of such paths.
        plugin_dirs: Vec<PathBuf>,
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
    let mut exit_after_first_frame = false;
    let mut open_sessions = false;
    let mut plugin_dirs = Vec::new();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => return Ok(Command::Help),
            "--version" | "-V" => return Ok(Command::Version),
            "--exit-after-first-frame" => exit_after_first_frame = true,
            "--open-sessions" => open_sessions = true,
            "--plugin-dir" => {
                let value = args
                    .next()
                    .ok_or_else(|| UsageError("--plugin-dir needs a path".into()))?;
                plugin_dirs.push(plugin_dir(&value)?);
            }
            other => match other.strip_prefix("--plugin-dir=") {
                Some(value) => plugin_dirs.push(plugin_dir(value)?),
                None => return Err(UsageError(format!("unknown argument: {other}"))),
            },
        }
    }
    if exit_after_first_frame && open_sessions {
        return Err(UsageError(
            "--open-sessions cannot be combined with --exit-after-first-frame".into(),
        ));
    }
    Ok(Command::Run {
        exit_after_first_frame,
        open_sessions,
        plugin_dirs,
    })
}

/// One `--plugin-dir` value. A build without plugin support refuses the option instead of ignoring it.
fn plugin_dir(value: &str) -> Result<PathBuf, UsageError> {
    if !cfg!(feature = "plugins") {
        return Err(UsageError(
            "--plugin-dir needs a build with plugin support (feature `plugins`)".into(),
        ));
    }
    if value.is_empty() {
        return Err(UsageError("--plugin-dir needs a path".into()));
    }
    Ok(PathBuf::from(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_strs(args: &[&str]) -> Result<Command, UsageError> {
        parse(args.iter().map(|s| (*s).to_owned()))
    }

    #[test]
    fn no_arguments_runs() {
        assert_eq!(
            parse_strs(&[]),
            Ok(Command::Run {
                exit_after_first_frame: false,
                open_sessions: false,
                plugin_dirs: Vec::new()
            })
        );
    }

    #[test]
    fn exit_after_first_frame() {
        assert_eq!(
            parse_strs(&["--exit-after-first-frame"]),
            Ok(Command::Run {
                exit_after_first_frame: true,
                open_sessions: false,
                plugin_dirs: Vec::new()
            })
        );
    }

    #[test]
    fn open_sessions() {
        assert_eq!(
            parse_strs(&["--open-sessions"]),
            Ok(Command::Run {
                exit_after_first_frame: false,
                open_sessions: true,
                plugin_dirs: Vec::new()
            })
        );
    }

    #[test]
    fn open_sessions_with_exit_after_first_frame_is_a_usage_error() {
        let expected = Err(UsageError(
            "--open-sessions cannot be combined with --exit-after-first-frame".to_owned(),
        ));
        assert_eq!(
            parse_strs(&["--open-sessions", "--exit-after-first-frame"]),
            expected
        );
        assert_eq!(
            parse_strs(&["--exit-after-first-frame", "--open-sessions"]),
            expected
        );
    }

    #[test]
    fn usage_lists_open_sessions() {
        assert!(USAGE.contains("--open-sessions"));
    }

    #[cfg(feature = "plugins")]
    #[test]
    fn plugin_dir_is_repeatable_and_keeps_the_order() {
        let got = parse_strs(&["--plugin-dir", "a", "--plugin-dir=b/c", "--plugin-dir", "d"]);
        assert_eq!(
            got,
            Ok(Command::Run {
                exit_after_first_frame: false,
                open_sessions: false,
                plugin_dirs: vec!["a".into(), "b/c".into(), "d".into()],
            })
        );
    }

    #[cfg(feature = "plugins")]
    #[test]
    fn plugin_dir_needs_a_value() {
        assert!(parse_strs(&["--plugin-dir"]).is_err());
        assert!(parse_strs(&["--plugin-dir="]).is_err());
        assert!(parse_strs(&["--plugin-dir", ""]).is_err());
    }

    #[cfg(not(feature = "plugins"))]
    #[test]
    fn plugin_dir_is_a_usage_error_without_the_feature() {
        let err = parse_strs(&["--plugin-dir", "a"]).unwrap_err();
        assert!(err.0.contains("plugin support"));
        assert!(parse_strs(&["--plugin-dir=a"]).is_err());
    }

    #[test]
    fn usage_lists_plugin_dir() {
        assert!(USAGE.contains("--plugin-dir"));
    }

    #[test]
    fn help_and_version() {
        assert_eq!(parse_strs(&["--help"]), Ok(Command::Help));
        assert_eq!(parse_strs(&["-h"]), Ok(Command::Help));
        assert_eq!(parse_strs(&["--version"]), Ok(Command::Version));
        assert_eq!(parse_strs(&["-V"]), Ok(Command::Version));
    }

    #[test]
    fn unknown_argument_is_a_usage_error() {
        assert_eq!(
            parse_strs(&["--nope"]),
            Err(UsageError("unknown argument: --nope".to_owned()))
        );
        assert!(parse_strs(&["--exit-after-first-frame", "x"]).is_err());
    }
}
