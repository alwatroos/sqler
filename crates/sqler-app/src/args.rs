// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Command-line argument parsing, with std only.

use std::fmt;

/// The usage text, printed for `--help` and after a usage error.
pub(crate) const USAGE: &str = "\
Usage:
  sqler [--exit-after-first-frame]
  sqler --help | -h
  sqler --version | -V

Options:
  --exit-after-first-frame   Exit with code 0 once the first frame is presented
  -h, --help                 Print this help
  -V, --version              Print the version
";

/// A parsed command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Command {
    /// Start the application.
    Run {
        /// Exit as soon as the first frame has been presented.
        exit_after_first_frame: bool,
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
    for arg in args {
        match arg.as_str() {
            "--help" | "-h" => return Ok(Command::Help),
            "--version" | "-V" => return Ok(Command::Version),
            "--exit-after-first-frame" => exit_after_first_frame = true,
            other => return Err(UsageError(format!("unknown argument: {other}"))),
        }
    }
    Ok(Command::Run {
        exit_after_first_frame,
    })
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
                exit_after_first_frame: false
            })
        );
    }

    #[test]
    fn exit_after_first_frame() {
        assert_eq!(
            parse_strs(&["--exit-after-first-frame"]),
            Ok(Command::Run {
                exit_after_first_frame: true
            })
        );
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
