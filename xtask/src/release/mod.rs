// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `release` task: build, sign and verify the update manifest (ADR-0066).

mod args;
mod build;
mod sign;

use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::process::ExitCode;

use args::{Command, ManifestArgs};

const MAX_PLAN_BYTES: u64 = 4 * 1024 * 1024;

/// Run `release <args>`; never panics. Exit codes: 0 ok, 1 failure, 2 usage error.
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
    match command {
        Command::Manifest(a) => manifest(a, w),
        Command::Verify {
            file,
            public_key_file,
        } => verify(file, public_key_file.as_deref(), w),
    }
}

fn write_error(err: &std::io::Error) -> String {
    format!("cannot write to standard output: {err}")
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
}

fn manifest(a: &ManifestArgs, w: &mut dyn Write) -> Result<(), String> {
    let mut plan = Vec::new();
    File::open(&a.plan)
        .and_then(|f| f.take(MAX_PLAN_BYTES + 1).read_to_end(&mut plan))
        .map_err(|_| "cannot read the dist plan".to_owned())?;
    if plan.len() as u64 > MAX_PLAN_BYTES {
        return Err("dist plan is too large".to_owned());
    }
    let opts = build::BuildOptions {
        tag: a.tag.clone(),
        published: a.published.clone(),
        partial: a.partial,
    };
    let built = build::build(&plan, &a.artifacts, &opts)?;
    let out_name = file_name(&a.out);
    let line = if let Some(key_file) = &a.key_file {
        let seed = sign::read_seed(key_file)?;
        let embedded = sqler_update::embedded_keys();
        let bytes = sign::sign(&built, &seed, a.test_key, &embedded)?;
        let kid = envelope_key_id(&bytes)?;
        std::fs::write(&a.out, &bytes).map_err(|_| "cannot write the output file".to_owned())?;
        format!(
            "signed {out_name} key_id={kid} version={} platforms={}{}",
            built.version,
            built.platforms.len(),
            if a.test_key { " (TEST ONLY key)" } else { "" }
        )
    } else {
        let mut bytes =
            serde_json::to_vec(&built).map_err(|_| "cannot encode manifest".to_owned())?;
        bytes.push(b'\n');
        std::fs::write(&a.out, &bytes).map_err(|_| "cannot write the output file".to_owned())?;
        format!(
            "unsigned manifest written {out_name} version={} platforms={}",
            built.version,
            built.platforms.len()
        )
    };
    writeln!(w, "{line}").map_err(|e| write_error(&e))
}

fn envelope_key_id(bytes: &[u8]) -> Result<String, String> {
    serde_json::from_slice::<sqler_update::Envelope>(bytes)
        .map(|e| e.key_id)
        .map_err(|_| "envelope is not valid JSON".to_owned())
}

fn verify(file: &Path, public_key_file: Option<&Path>, w: &mut dyn Write) -> Result<(), String> {
    let (kid, m) = sign::verify_file(file, public_key_file)?;
    writeln!(
        w,
        "ok key_id={kid} version={} published={} notes_url={}",
        m.version, m.published, m.notes_url
    )
    .map_err(|e| write_error(&e))?;
    for (triple, d) in &m.platforms {
        writeln!(w, "{triple} {} {} {}", d.size, d.sha256, d.url).map_err(|e| write_error(&e))?;
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;

    use super::args::{Command, ManifestArgs};
    use super::build::tests::{PLAN, archives};
    use super::execute;

    /// TEST ONLY, never a release key.
    const SEED: [u8; 32] = [0xC3; 32];

    fn manifest_args(dir: &std::path::Path, key: Option<std::path::PathBuf>) -> ManifestArgs {
        let plan = dir.join("plan.json");
        std::fs::write(&plan, PLAN).unwrap();
        ManifestArgs {
            plan,
            artifacts: dir.to_path_buf(),
            published: "2026-10-24T12:00:00Z".to_owned(),
            out: dir.join("sqler-update.TEST-ONLY.json"),
            tag: Some("v1.2.3".to_owned()),
            test_key: key.is_some(),
            key_file: key,
            partial: false,
        }
    }

    #[test]
    fn manifest_then_verify_end_to_end() {
        let d = archives("e2e");
        let key = d.0.join("key");
        std::fs::write(&key, STANDARD.encode(SEED)).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let args = manifest_args(&d.0, Some(key));
        let mut out = Vec::new();
        execute(&Command::Manifest(args), &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.starts_with("signed sqler-update.TEST-ONLY.json key_id="),
            "{text}"
        );
        assert!(
            text.trim_end().ends_with("platforms=5 (TEST ONLY key)"),
            "{text}"
        );
        assert!(!text.contains(std::env::temp_dir().to_string_lossy().as_ref()));

        let pair = ring::signature::Ed25519KeyPair::from_seed_unchecked(&SEED).unwrap();
        let pubf = d.0.join("test.pub");
        let public = ring::signature::KeyPair::public_key(&pair);
        std::fs::write(&pubf, STANDARD.encode(public.as_ref())).unwrap();
        let mut out = Vec::new();
        execute(
            &Command::Verify {
                file: d.0.join("sqler-update.TEST-ONLY.json"),
                public_key_file: Some(pubf),
            },
            &mut out,
        )
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("ok key_id="), "{text}");
        assert_eq!(text.lines().count(), 6, "{text}");
    }

    #[test]
    fn unsigned_manifest_is_bare_json() {
        let d = archives("unsigned");
        let mut args = manifest_args(&d.0, None);
        args.out = d.0.join("m.json");
        let mut out = Vec::new();
        execute(&Command::Manifest(args), &mut out).unwrap();
        assert!(
            String::from_utf8(out)
                .unwrap()
                .starts_with("unsigned manifest written m.json")
        );
        let m: sqler_update::Manifest =
            serde_json::from_slice(&std::fs::read(d.0.join("m.json")).unwrap()).unwrap();
        assert_eq!(m.platforms.len(), 5);
    }

    #[test]
    fn no_file_is_written_when_signing_fails() {
        let d = archives("nofile");
        let key = d.0.join("key");
        std::fs::write(&key, "not a key").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let args = manifest_args(&d.0, Some(key));
        let out = args.out.clone();
        assert!(execute(&Command::Manifest(args), &mut Vec::new()).is_err());
        assert!(!out.exists());
    }
}
