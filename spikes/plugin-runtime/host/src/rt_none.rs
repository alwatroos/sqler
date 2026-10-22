// SPDX-License-Identifier: Apache-2.0
//! The `base` variant: the same CLI without an engine.

use std::path::Path;

use crate::cli::RunCommand;

const NO_ENGINE: &str = "no engine: this is the base variant";

pub fn run_fixture(_path: &Path) -> Result<(), String> {
    Err(NO_ENGINE.to_owned())
}

pub fn bench(
    _path: &Path,
    _instances: usize,
    _samples: usize,
    _calls: usize,
) -> Result<(), String> {
    Err(NO_ENGINE.to_owned())
}

pub fn precompile(_input: &Path, _output: &Path, _pulley: bool) -> Result<(), String> {
    Err(NO_ENGINE.to_owned())
}

pub fn run_command(_rc: &RunCommand) -> Result<u8, String> {
    Err(NO_ENGINE.to_owned())
}
