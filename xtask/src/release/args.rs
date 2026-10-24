// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Argument parsing for `release`.

use std::path::PathBuf;

/// Arguments of `release manifest`.
#[derive(Debug, PartialEq, Eq)]
pub struct ManifestArgs {
    /// The dist plan manifest (`plan-dist-manifest.json`).
    pub plan: PathBuf,
    /// Directory holding the final archives.
    pub artifacts: PathBuf,
    /// `YYYY-MM-DDTHH:MM:SSZ`.
    pub published: String,
    /// Output file; its parent directory must exist.
    pub out: PathBuf,
    /// The release tag, `v<version>`.
    pub tag: Option<String>,
    /// File holding the base64 Ed25519 seed.
    pub key_file: Option<PathBuf>,
    /// The key is a TEST ONLY key.
    pub test_key: bool,
    /// Skip archives missing on disk (dry runs).
    pub partial: bool,
}

/// A parsed subcommand.
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    /// Build (and sign) the manifest.
    Manifest(ManifestArgs),
    /// Verify an envelope file.
    Verify {
        /// The envelope file.
        file: PathBuf,
        /// A public key file (base64) to trust instead of the compiled-in keys.
        public_key_file: Option<PathBuf>,
    },
}

/// A usage error (exit code 2).
#[derive(Debug, PartialEq, Eq)]
pub struct UsageError(pub String);

/// True for `YYYY-MM-DDTHH:MM:SSZ` with plausible ranges.
fn is_published(text: &str) -> bool {
    let b = text.as_bytes();
    if b.len() != 20 {
        return false;
    }
    let digits = |r: std::ops::Range<usize>| b[r].iter().all(u8::is_ascii_digit);
    let num = |r: std::ops::Range<usize>| text[r].parse::<u32>().unwrap_or(u32::MAX);
    digits(0..4)
        && b[4] == b'-'
        && digits(5..7)
        && b[7] == b'-'
        && digits(8..10)
        && b[10] == b'T'
        && digits(11..13)
        && b[13] == b':'
        && digits(14..16)
        && b[16] == b':'
        && digits(17..19)
        && b[19] == b'Z'
        && (1..=12).contains(&num(5..7))
        && (1..=31).contains(&num(8..10))
        && num(11..13) <= 23
        && num(14..16) <= 59
        && num(17..19) <= 60
}

fn value<'a>(it: &mut std::slice::Iter<'a, String>, flag: &str) -> Result<&'a String, UsageError> {
    it.next()
        .ok_or_else(|| UsageError(format!("flag `{flag}` needs a value")))
}

fn set<T>(slot: &mut Option<T>, flag: &str, v: T) -> Result<(), UsageError> {
    if slot.is_some() {
        return Err(UsageError(format!("flag `{flag}` given twice")));
    }
    *slot = Some(v);
    Ok(())
}

fn flag_once(slot: &mut bool, flag: &str) -> Result<(), UsageError> {
    if *slot {
        return Err(UsageError(format!("flag `{flag}` given twice")));
    }
    *slot = true;
    Ok(())
}

fn required<T>(slot: Option<T>, flag: &str) -> Result<T, UsageError> {
    slot.ok_or_else(|| UsageError(format!("missing required flag `{flag}`")))
}

fn parse_manifest(rest: &[String]) -> Result<ManifestArgs, UsageError> {
    let (mut plan, mut artifacts, mut published, mut out) = (None, None, None, None);
    let (mut tag, mut key_file) = (None, None);
    let (mut test_key, mut partial) = (false, false);
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--plan" => set(
                &mut plan,
                "--plan",
                PathBuf::from(value(&mut it, "--plan")?),
            )?,
            "--artifacts" => set(
                &mut artifacts,
                "--artifacts",
                PathBuf::from(value(&mut it, "--artifacts")?),
            )?,
            "--published" => set(
                &mut published,
                "--published",
                value(&mut it, "--published")?.clone(),
            )?,
            "--out" => set(&mut out, "--out", PathBuf::from(value(&mut it, "--out")?))?,
            "--tag" => set(&mut tag, "--tag", value(&mut it, "--tag")?.clone())?,
            "--key-file" => set(
                &mut key_file,
                "--key-file",
                PathBuf::from(value(&mut it, "--key-file")?),
            )?,
            "--test-key" => flag_once(&mut test_key, "--test-key")?,
            "--partial" => flag_once(&mut partial, "--partial")?,
            other => return Err(UsageError(format!("unknown argument `{other}`"))),
        }
    }
    let published = required(published, "--published")?;
    if !is_published(&published) {
        return Err(UsageError(
            "`--published` must look like 2026-01-31T12:00:00Z".to_owned(),
        ));
    }
    if test_key && key_file.is_none() {
        return Err(UsageError("`--test-key` needs `--key-file`".to_owned()));
    }
    if partial && key_file.is_some() && !test_key {
        return Err(UsageError(
            "`--partial` with `--key-file` needs `--test-key`: a production manifest is complete"
                .to_owned(),
        ));
    }
    Ok(ManifestArgs {
        plan: required(plan, "--plan")?,
        artifacts: required(artifacts, "--artifacts")?,
        published,
        out: required(out, "--out")?,
        tag,
        key_file,
        test_key,
        partial,
    })
}

fn parse_verify(rest: &[String]) -> Result<Command, UsageError> {
    let mut file: Option<PathBuf> = None;
    let mut public_key_file = None;
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        if arg == "--public-key-file" {
            set(
                &mut public_key_file,
                "--public-key-file",
                PathBuf::from(value(&mut it, "--public-key-file")?),
            )?;
        } else if arg.starts_with('-') {
            return Err(UsageError(format!("unknown argument `{arg}`")));
        } else if file.is_some() {
            return Err(UsageError(format!("unexpected argument `{arg}`")));
        } else {
            file = Some(PathBuf::from(arg));
        }
    }
    Ok(Command::Verify {
        file: required(file, "<envelope-file>")?,
        public_key_file,
    })
}

/// Parse the arguments after `release`.
pub fn parse(args: &[String]) -> Result<Command, UsageError> {
    let Some((sub, rest)) = args.split_first() else {
        return Err(UsageError(
            "missing subcommand, one of: manifest, verify".to_owned(),
        ));
    };
    match sub.as_str() {
        "manifest" => parse_manifest(rest).map(Command::Manifest),
        "verify" => parse_verify(rest),
        other => Err(UsageError(format!(
            "unknown subcommand `{other}`, one of: manifest, verify"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::{Command, UsageError, parse};

    fn p(args: &[&str]) -> Result<Command, UsageError> {
        parse(&args.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>())
    }

    const BASE: [&str; 9] = [
        "manifest",
        "--plan",
        "p.json",
        "--artifacts",
        "dir",
        "--published",
        "2026-10-24T12:00:00Z",
        "--out",
        "o.json",
    ];

    fn with(extra: &[&str]) -> Result<Command, UsageError> {
        let mut v: Vec<&str> = BASE.to_vec();
        v.extend_from_slice(extra);
        p(&v)
    }

    #[test]
    fn minimal_manifest_parses() {
        let Ok(Command::Manifest(a)) = with(&[]) else {
            panic!("expected manifest");
        };
        assert_eq!(a.published, "2026-10-24T12:00:00Z");
        assert!(a.key_file.is_none() && !a.test_key && !a.partial && a.tag.is_none());
    }

    #[test]
    fn full_manifest_parses() {
        let r = with(&[
            "--tag",
            "v1.0.0",
            "--key-file",
            "k",
            "--test-key",
            "--partial",
        ]);
        let Ok(Command::Manifest(a)) = r else {
            panic!("expected manifest");
        };
        assert!(a.test_key && a.partial);
        assert_eq!(a.tag.as_deref(), Some("v1.0.0"));
    }

    #[test]
    fn usage_errors() {
        assert!(p(&[]).is_err());
        assert!(p(&["bogus"]).is_err());
        assert!(with(&["--nope"]).is_err());
        assert!(with(&["--tag", "v1", "--tag", "v2"]).is_err());
        assert!(with(&["--partial", "--partial"]).is_err());
        assert!(with(&["--tag"]).is_err());
        assert!(with(&["--test-key"]).is_err());
        assert!(with(&["--partial", "--key-file", "k"]).is_err());
        assert!(with(&["--partial", "--key-file", "k", "--test-key"]).is_ok());
        assert!(with(&["--partial"]).is_ok());
    }

    #[test]
    fn missing_required_flags() {
        for skip in [1usize, 3, 5, 7] {
            let mut v: Vec<&str> = BASE.to_vec();
            v.drain(skip..skip + 2);
            assert!(p(&v).is_err(), "missing flag at {skip}");
        }
    }

    #[test]
    fn bad_published_shapes() {
        for bad in [
            "2026-10-24",
            "2026-10-24T12:00:00",
            "2026-10-24 12:00:00Z",
            "2026-13-24T12:00:00Z",
            "2026-10-24T25:00:00Z",
            "2026-10-24T12:00:00+00:00",
            "20x6-10-24T12:00:00Z",
        ] {
            let mut v: Vec<&str> = BASE.to_vec();
            v[6] = bad;
            assert!(p(&v).is_err(), "{bad}");
        }
    }

    #[test]
    fn verify_forms() {
        assert_eq!(
            p(&["verify", "e.json"]),
            Ok(Command::Verify {
                file: "e.json".into(),
                public_key_file: None
            })
        );
        assert!(matches!(
            p(&["verify", "e.json", "--public-key-file", "k.pub"]),
            Ok(Command::Verify {
                public_key_file: Some(_),
                ..
            })
        ));
        assert!(p(&["verify"]).is_err());
        assert!(p(&["verify", "a", "b"]).is_err());
        assert!(p(&["verify", "a", "--x"]).is_err());
        assert!(p(&["verify", "a", "--public-key-file"]).is_err());
        assert!(
            p(&[
                "verify",
                "a",
                "--public-key-file",
                "k",
                "--public-key-file",
                "k"
            ])
            .is_err()
        );
    }
}
