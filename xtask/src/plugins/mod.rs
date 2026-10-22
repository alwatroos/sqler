// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `plugins` task: build, check, list and clean the guest workspace in `plugins/`.

mod args;
mod build;
mod component;
mod table;

use std::io::Write;
use std::process::ExitCode;

use args::Command;

/// Run `plugins <args>`; never panics. Exit codes: 0 ok, 1 failure, 2 usage error.
pub fn run(args: &[String]) -> ExitCode {
    match args::parse(args) {
        Err(err) => {
            eprintln!("error: {}", err.0);
            ExitCode::from(2)
        }
        Ok(command) => match execute(&command, &mut std::io::stdout().lock()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(message) => {
                eprintln!("error: {message}");
                ExitCode::from(1)
            }
        },
    }
}

fn execute(command: &Command, w: &mut dyn Write) -> Result<(), String> {
    let root = build::repo_root();
    match command {
        Command::Build { release, names } => build::build(&root, *release, names, w),
        Command::Check => build::check(&root),
        Command::List => list(w),
        Command::Clean => build::clean(&root),
    }
}

fn list(w: &mut dyn Write) -> Result<(), String> {
    for row in table::PLUGINS {
        writeln!(w, "{} {} {}", row.name, row.dir, row.package.unwrap_or("-"))
            .map_err(|e| build::write_error(&e))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::{Error, ErrorKind, Write};

    use super::list;

    struct ClosedPipe;

    impl Write for ClosedPipe {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(Error::from(ErrorKind::BrokenPipe))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_closed_pipe_is_an_error_not_a_panic() {
        let err = list(&mut ClosedPipe).err().unwrap_or_default();
        assert!(err.contains("standard output"), "{err}");
    }

    #[test]
    fn list_writes_one_line_per_row() {
        let mut buf = Vec::new();
        assert!(list(&mut buf).is_ok());
        assert!(String::from_utf8_lossy(&buf).starts_with("smoke plugins/fixtures/smoke "));
    }
}
