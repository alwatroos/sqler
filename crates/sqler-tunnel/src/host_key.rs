// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use russh::client;
use russh::keys::known_hosts::check_known_hosts_path;
use russh::keys::{Error as KeyError, HashAlg, PublicKey, PublicKeyOrCertificate};

use crate::error::TunnelError;

/// A server host key, as shown to the user and as recorded in `known_hosts`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostKeyInfo {
    /// SSH server host the key was presented for.
    pub host: String,
    /// SSH server port.
    pub port: u16,
    /// Key algorithm, for example `ssh-ed25519`.
    pub algorithm: String,
    /// `SHA256:` fingerprint in the same form as `ssh-keygen -l`.
    pub fingerprint: String,
    /// The public key as an `OpenSSH` one-line string.
    pub public_key: String,
}

impl HostKeyInfo {
    fn new(host: &str, port: u16, key: &PublicKey) -> Result<Self, TunnelError> {
        let public_key = key
            .to_openssh()
            .map_err(|e| TunnelError::Ssh(format!("cannot encode the host key: {e}")))?;
        Ok(Self {
            host: host.to_owned(),
            port,
            algorithm: key.algorithm().to_string(),
            fingerprint: key.fingerprint(HashAlg::Sha256).to_string(),
            public_key,
        })
    }
}

/// Appends `key` to the `known_hosts` file (`[host]:port` form when the port is not 22).
/// Creates the file and its parent directory.
///
/// Call this only after the user confirmed the fingerprint.
///
/// # Errors
/// [`TunnelError::KnownHosts`] when the key text is invalid or the file cannot be written.
pub fn trust_host_key(known_hosts: &Path, key: &HostKeyInfo) -> Result<(), TunnelError> {
    let fail = |message: String| TunnelError::KnownHosts {
        path: known_hosts.to_path_buf(),
        message,
    };
    let public = PublicKey::from_openssh(&key.public_key)
        .map_err(|e| fail(format!("invalid public key: {e}")))?;
    // Not russh's `learn_known_hosts_path`: it writes a blank first line into an empty file,
    // which shifts the line numbers `HostKeyChanged` reports.
    append_entry(known_hosts, &key.host, key.port, &public).map_err(|e| fail(e.to_string()))
}

fn append_entry(path: &Path, host: &str, port: u16, key: &PublicKey) -> std::io::Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .open(path)?;
    let mut line = String::new();
    // A file that does not end in a newline gets one first; an empty file gets none.
    if file.seek(SeekFrom::End(-1)).is_ok() {
        let mut last = [0u8; 1];
        file.read_exact(&mut last)?;
        if last[0] != b'\n' {
            line.push('\n');
        }
    }
    let host_port = if port == 22 {
        host.to_owned()
    } else {
        format!("[{host}]:{port}")
    };
    line.push_str(&host_port);
    let openssh = key.to_openssh().map_err(std::io::Error::other)?;
    line.push(' ');
    line.push_str(openssh.trim());
    line.push('\n');
    file.seek(SeekFrom::End(0))?;
    file.write_all(line.as_bytes())
}

/// The russh client handler that verifies the server key against `known_hosts`.
pub(crate) struct HostKeyHandler {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) known_hosts: PathBuf,
}

impl HostKeyHandler {
    fn check(&self, key: &PublicKey) -> Result<bool, TunnelError> {
        // russh maps every open failure to "no entries"; only a missing file means "unknown".
        if let Err(e) = std::fs::File::open(&self.known_hosts)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            return Err(TunnelError::KnownHosts {
                path: self.known_hosts.clone(),
                message: e.to_string(),
            });
        }
        match check_known_hosts_path(&self.host, self.port, key, &self.known_hosts) {
            Ok(true) => Ok(true),
            Ok(false) => Err(TunnelError::UnknownHostKey(HostKeyInfo::new(
                &self.host, self.port, key,
            )?)),
            Err(KeyError::KeyChanged { line }) => Err(TunnelError::HostKeyChanged {
                key: HostKeyInfo::new(&self.host, self.port, key)?,
                line,
            }),
            Err(e) => Err(TunnelError::KnownHosts {
                path: self.known_hosts.clone(),
                message: e.to_string(),
            }),
        }
    }
}

impl client::Handler for HostKeyHandler {
    type Error = TunnelError;

    fn check_server_key(
        &mut self,
        server_public_key: &PublicKeyOrCertificate,
    ) -> impl Future<Output = Result<bool, Self::Error>> + Send {
        std::future::ready(self.check(&server_public_key.public_key()))
    }
}

#[cfg(test)]
mod tests {
    use russh::keys::{Algorithm, PrivateKey};

    use super::*;

    fn temp(test: &str) -> PathBuf {
        std::env::temp_dir().join(format!("sqler-tunnel-{}-{test}", std::process::id()))
    }

    fn random_key() -> PublicKey {
        PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)
            .unwrap()
            .public_key()
            .clone()
    }

    fn handler(path: PathBuf, port: u16) -> HostKeyHandler {
        HostKeyHandler {
            host: "bastion.test".into(),
            port,
            known_hosts: path,
        }
    }

    #[test]
    fn missing_file_is_unknown_key() {
        let dir = temp("missing");
        let key = random_key();
        let err = handler(dir.join("nope"), 22).check(&key).unwrap_err();
        let TunnelError::UnknownHostKey(info) = err else {
            panic!("expected UnknownHostKey");
        };
        assert!(info.fingerprint.starts_with("SHA256:"));
        assert_eq!(info.algorithm, "ssh-ed25519");
        assert_eq!(info.host, "bastion.test");
    }

    #[test]
    fn trust_then_match_default_and_custom_port() {
        for port in [22, 2222] {
            let dir = temp(&format!("trust-{port}"));
            let path = dir.join("sub").join("known_hosts");
            let key = random_key();
            let info = HostKeyInfo::new("bastion.test", port, &key).unwrap();
            trust_host_key(&path, &info).unwrap();
            assert!(handler(path.clone(), port).check(&key).unwrap());
            let text = std::fs::read_to_string(&path).unwrap();
            if port == 22 {
                assert!(text.starts_with("bastion.test ssh-ed25519 "), "{text}");
            } else {
                assert!(
                    text.starts_with("[bastion.test]:2222 ssh-ed25519 "),
                    "{text}"
                );
            }
            std::fs::remove_dir_all(&dir).unwrap();
        }
    }

    #[test]
    fn changed_key_reports_line() {
        let dir = temp("changed");
        let path = dir.join("known_hosts");
        let first = HostKeyInfo::new("other.test", 22, &random_key()).unwrap();
        trust_host_key(&path, &first).unwrap();
        let old = HostKeyInfo::new("bastion.test", 22, &random_key()).unwrap();
        trust_host_key(&path, &old).unwrap();
        let err = handler(path, 22).check(&random_key()).unwrap_err();
        assert!(
            matches!(err, TunnelError::HostKeyChanged { line: 2, .. }),
            "{err:?}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn malformed_entry_is_known_hosts_error() {
        let dir = temp("malformed");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("known_hosts");
        std::fs::write(&path, "bastion.test ssh-ed25519 !!!notbase64!!!\n").unwrap();
        let err = handler(path, 22).check(&random_key()).unwrap_err();
        assert!(matches!(err, TunnelError::KnownHosts { .. }), "{err:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unreadable_known_hosts_is_known_hosts_error() {
        let dir = temp("unreadable");
        std::fs::create_dir_all(&dir).unwrap();
        // A directory cannot be read as a file; this fails on every platform.
        let err = handler(dir.clone(), 22).check(&random_key()).unwrap_err();
        assert!(matches!(err, TunnelError::KnownHosts { .. }), "{err:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn trust_rejects_bad_key_text() {
        let mut info = HostKeyInfo::new("h", 22, &random_key()).unwrap();
        info.public_key = "garbage".into();
        let err = trust_host_key(&temp("badtext").join("kh"), &info).unwrap_err();
        assert!(matches!(err, TunnelError::KnownHosts { .. }));
    }
}
