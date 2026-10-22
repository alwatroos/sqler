// SPDX-License-Identifier: Apache-2.0
//! Argument parsing (std only, no clap). A parse error is a usage error (exit code 2).

use std::net::SocketAddr;
use std::path::PathBuf;

/// Defaults of `bench`.
pub const DEFAULT_INSTANCES: usize = 100;
/// Defaults of `bench`.
pub const DEFAULT_SAMPLES: usize = 200;
/// Defaults of `bench`.
pub const DEFAULT_CALLS: usize = 10_000;

/// A parsed command line.
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Info,
    RunFixture(PathBuf),
    Bench {
        path: PathBuf,
        instances: usize,
        samples: usize,
        calls: usize,
    },
    Precompile {
        input: PathBuf,
        output: PathBuf,
        pulley: bool,
    },
    RunCommand(RunCommand),
}

/// Arguments of `run-command`.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct RunCommand {
    pub guest: PathBuf,
    pub env: Vec<(String, String)>,
    pub allow_tcp: Vec<SocketAddr>,
    pub dirs: Vec<(PathBuf, String)>,
}

pub const USAGE: &str = "usage: plugin-spike-host <command>\n\
  info\n\
  run-fixture <fixture.wasm|.cwasm>\n\
  bench <fixture.wasm|.cwasm> [--instances N] [--samples N] [--calls N]\n\
  precompile <in.wasm> <out.cwasm> [--pulley]\n\
  run-command <guest.wasm> [--env K=V]... [--allow-tcp IP:PORT]... [--dir HOST::GUEST]...";

fn positive(flag: &str, value: &str) -> Result<usize, String> {
    match value.parse::<usize>() {
        Ok(n) if n > 0 => Ok(n),
        _ => Err(format!("{flag} needs a positive integer, got `{value}`")),
    }
}

fn value<'a>(flag: &str, it: &mut impl Iterator<Item = &'a String>) -> Result<&'a String, String> {
    it.next().ok_or_else(|| format!("{flag} needs a value"))
}

/// Parses the arguments after the program name.
pub fn parse(args: &[String]) -> Result<Command, String> {
    let mut it = args.iter();
    let cmd = it.next().ok_or("missing command")?;
    match cmd.as_str() {
        "info" => match it.next() {
            None => Ok(Command::Info),
            Some(extra) => Err(format!("unexpected argument `{extra}`")),
        },
        "run-fixture" => {
            let path = it.next().ok_or("run-fixture needs a fixture path")?;
            match it.next() {
                None => Ok(Command::RunFixture(PathBuf::from(path))),
                Some(extra) => Err(format!("unexpected argument `{extra}`")),
            }
        }
        "bench" => parse_bench(it),
        "precompile" => parse_precompile(it),
        "run-command" => parse_run_command(it),
        other => Err(format!("unknown command `{other}`")),
    }
}

fn parse_bench<'a>(mut it: impl Iterator<Item = &'a String>) -> Result<Command, String> {
    let path = PathBuf::from(it.next().ok_or("bench needs a fixture path")?);
    let (mut instances, mut samples, mut calls) =
        (DEFAULT_INSTANCES, DEFAULT_SAMPLES, DEFAULT_CALLS);
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--instances" => instances = positive(flag, value(flag, &mut it)?)?,
            "--samples" => samples = positive(flag, value(flag, &mut it)?)?,
            "--calls" => calls = positive(flag, value(flag, &mut it)?)?,
            other => return Err(format!("unknown option `{other}`")),
        }
    }
    Ok(Command::Bench {
        path,
        instances,
        samples,
        calls,
    })
}

fn parse_precompile<'a>(mut it: impl Iterator<Item = &'a String>) -> Result<Command, String> {
    let input = PathBuf::from(it.next().ok_or("precompile needs <in.wasm> <out.cwasm>")?);
    let output = PathBuf::from(it.next().ok_or("precompile needs <in.wasm> <out.cwasm>")?);
    let mut pulley = false;
    for flag in it {
        match flag.as_str() {
            "--pulley" => pulley = true,
            other => return Err(format!("unknown option `{other}`")),
        }
    }
    Ok(Command::Precompile {
        input,
        output,
        pulley,
    })
}

fn parse_run_command<'a>(mut it: impl Iterator<Item = &'a String>) -> Result<Command, String> {
    let mut rc = RunCommand {
        guest: PathBuf::from(it.next().ok_or("run-command needs a guest path")?),
        ..RunCommand::default()
    };
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--env" => {
                let kv = value(flag, &mut it)?;
                let (k, v) = kv
                    .split_once('=')
                    .ok_or_else(|| format!("--env needs K=V, got `{kv}`"))?;
                if k.is_empty() {
                    return Err(format!("--env needs a key, got `{kv}`"));
                }
                rc.env.push((k.to_owned(), v.to_owned()));
            }
            "--allow-tcp" => {
                let a = value(flag, &mut it)?;
                let addr = a
                    .parse::<SocketAddr>()
                    .map_err(|_| format!("--allow-tcp needs IP:PORT, got `{a}`"))?;
                rc.allow_tcp.push(addr);
            }
            "--dir" => {
                let d = value(flag, &mut it)?;
                let (host, guest) = d
                    .split_once("::")
                    .ok_or_else(|| format!("--dir needs HOST::GUEST, got `{d}`"))?;
                if host.is_empty() || guest.is_empty() {
                    return Err(format!("--dir needs HOST::GUEST, got `{d}`"));
                }
                rc.dirs.push((PathBuf::from(host), guest.to_owned()));
            }
            other => return Err(format!("unknown option `{other}`")),
        }
    }
    Ok(Command::RunCommand(rc))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> Result<Command, String> {
        let v: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
        parse(&v)
    }

    #[test]
    fn info_and_fixture() {
        assert_eq!(p(&["info"]), Ok(Command::Info));
        assert_eq!(
            p(&["run-fixture", "a.wasm"]),
            Ok(Command::RunFixture("a.wasm".into()))
        );
        assert!(p(&["run-fixture"]).is_err());
        assert!(p(&["info", "x"]).is_err());
    }

    #[test]
    fn missing_or_unknown_command_is_a_usage_error() {
        assert!(p(&[]).is_err());
        assert!(p(&["nope"]).is_err());
    }

    #[test]
    fn bench_defaults_and_flags() {
        assert_eq!(
            p(&["bench", "f.wasm"]),
            Ok(Command::Bench {
                path: "f.wasm".into(),
                instances: 100,
                samples: 200,
                calls: 10_000
            })
        );
        assert_eq!(
            p(&[
                "bench",
                "f.wasm",
                "--instances",
                "5",
                "--samples",
                "6",
                "--calls",
                "7"
            ]),
            Ok(Command::Bench {
                path: "f.wasm".into(),
                instances: 5,
                samples: 6,
                calls: 7
            })
        );
        assert!(p(&["bench", "f.wasm", "--samples", "0"]).is_err());
        assert!(p(&["bench", "f.wasm", "--samples"]).is_err());
        assert!(p(&["bench", "f.wasm", "--bogus"]).is_err());
    }

    #[test]
    fn precompile_flags() {
        assert_eq!(
            p(&["precompile", "a", "b", "--pulley"]),
            Ok(Command::Precompile {
                input: "a".into(),
                output: "b".into(),
                pulley: true
            })
        );
        assert!(p(&["precompile", "a"]).is_err());
    }

    #[test]
    fn run_command_flags() {
        let c = p(&[
            "run-command",
            "g.wasm",
            "--env",
            "A=b=c",
            "--allow-tcp",
            "127.0.0.1:14339",
            "--dir",
            "/x::/ca",
        ]);
        let Ok(Command::RunCommand(rc)) = c else {
            panic!("parse failed")
        };
        assert_eq!(rc.env, vec![("A".to_owned(), "b=c".to_owned())]);
        assert_eq!(rc.allow_tcp.len(), 1);
        assert_eq!(rc.dirs, vec![(PathBuf::from("/x"), "/ca".to_owned())]);
        assert!(p(&["run-command", "g.wasm", "--env", "nokv"]).is_err());
        assert!(p(&["run-command", "g.wasm", "--allow-tcp", "host:1"]).is_err());
        assert!(p(&["run-command", "g.wasm", "--dir", "only"]).is_err());
    }
}
