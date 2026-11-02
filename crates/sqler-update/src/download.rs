// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Download of the signed release artifact (ADR-0078).
//!
//! The file is streamed into `<dir>/<name>.part` while it is hashed, checked
//! against the signed size and sha256, and only then renamed to `<dir>/<name>`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use reqwest::Url;
use ring::digest::{Context, SHA256};
use tokio::io::AsyncWriteExt as _;

use crate::client::{UpdateClient, UpdateOffer, current_target};
use crate::error::from_reqwest;
use crate::manifest::Download;
use crate::{MAX_ARTIFACT_BYTES, UpdateError};

/// Which artifact of a release to fetch and install.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactKind {
    /// The `.tar.xz` (`.zip` on Windows) archive that holds the program.
    Archive,
    /// The macOS `.dmg` that holds `SQLer.app`.
    MacBundle,
}

/// A downloaded artifact whose size and sha256 matched the signed manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedArtifact {
    /// The version the manifest announced.
    pub version: semver::Version,
    /// What the file is.
    pub kind: ArtifactKind,
    /// Where the file is.
    pub path: PathBuf,
    /// Lowercase hex sha256 of the file.
    pub sha256: String,
    /// Size in bytes.
    pub size: u64,
}

/// The release file name of `target` for `kind`.
#[must_use]
pub fn artifact_file_name(target: &str, kind: ArtifactKind) -> String {
    let ext = match kind {
        ArtifactKind::MacBundle => "dmg",
        ArtifactKind::Archive if target.contains("-pc-windows-") => "zip",
        ArtifactKind::Archive => "tar.xz",
    };
    format!("sqler-app-{target}.{ext}")
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Checks that `path` is a regular file of `size` bytes with the given sha256.
/// It blocks while it reads the file.
///
/// # Errors
///
/// [`UpdateError::Io`] when the file cannot be read, [`UpdateError::SizeMismatch`]
/// or [`UpdateError::HashMismatch`] when it differs from the signed values.
pub fn verify_file(path: &Path, sha256: &str, size: u64) -> Result<(), UpdateError> {
    use std::io::Read as _;
    let meta = std::fs::symlink_metadata(path).map_err(|_| UpdateError::Io)?;
    if !meta.is_file() {
        return Err(UpdateError::Io);
    }
    if meta.len() != size {
        return Err(UpdateError::SizeMismatch);
    }
    let mut file = std::fs::File::open(path).map_err(|_| UpdateError::Io)?;
    let mut ctx = Context::new(&SHA256);
    let mut buf = vec![0_u8; 64 * 1024];
    let mut total = 0_u64;
    loop {
        let n = file.read(&mut buf).map_err(|_| UpdateError::Io)?;
        if n == 0 {
            break;
        }
        total += n as u64;
        ctx.update(&buf[..n]);
    }
    if total != size {
        return Err(UpdateError::SizeMismatch);
    }
    if hex(ctx.finish().as_ref()) == sha256 {
        Ok(())
    } else {
        Err(UpdateError::HashMismatch)
    }
}

impl UpdateClient {
    /// Downloads the artifact of `kind` for this platform into `dir` and
    /// verifies it against the signed size and sha256.
    ///
    /// A file in `dir` that already passes [`verify_file`] is reused without a
    /// request. Other entries of `dir` are removed. `progress` gets
    /// `(received, total)` for every chunk. A failure removes the partial file.
    ///
    /// # Errors
    ///
    /// [`UpdateError::NoArtifact`], [`UpdateError::UntrustedUrl`] and
    /// [`UpdateError::TooLarge`] before any request; transport errors,
    /// [`UpdateError::SizeMismatch`] and [`UpdateError::HashMismatch`] while
    /// downloading; [`UpdateError::Io`] for local file errors.
    pub async fn download(
        &self,
        offer: &UpdateOffer,
        kind: ArtifactKind,
        dir: &Path,
        progress: &(dyn Fn(u64, u64) + Send + Sync),
    ) -> Result<VerifiedArtifact, UpdateError> {
        let entry: &Download = match kind {
            ArtifactKind::Archive => offer.download.as_ref(),
            ArtifactKind::MacBundle => offer.bundle.as_ref(),
        }
        .ok_or(UpdateError::NoArtifact)?;
        let target = current_target().ok_or(UpdateError::NoArtifact)?;
        let name = artifact_file_name(target, kind);
        let url = self.trusted_url(&entry.url, &offer.version, &name)?;
        if entry.size > MAX_ARTIFACT_BYTES {
            return Err(UpdateError::TooLarge);
        }

        std::fs::create_dir_all(dir).map_err(|_| UpdateError::Io)?;
        clear_others(dir, &name)?;
        let path = dir.join(&name);
        let done = VerifiedArtifact {
            version: offer.version.clone(),
            kind,
            path: path.clone(),
            sha256: entry.sha256.clone(),
            size: entry.size,
        };
        if verify_file(&path, &entry.sha256, entry.size).is_ok() {
            return Ok(done);
        }
        let _ = std::fs::remove_file(&path);

        let part = dir.join(format!("{name}.part"));
        let result = self.fetch(url, &part, entry, progress).await;
        if result.is_err() {
            let _ = tokio::fs::remove_file(&part).await;
            return result.map(|()| done);
        }
        std::fs::rename(&part, &path).map_err(|_| {
            let _ = std::fs::remove_file(&part);
            UpdateError::Io
        })?;
        Ok(done)
    }

    async fn fetch(
        &self,
        url: Url,
        part: &Path,
        entry: &Download,
        progress: &(dyn Fn(u64, u64) + Send + Sync),
    ) -> Result<(), UpdateError> {
        let size = entry.size;
        let mut resp = tokio::time::timeout(self.stall, self.client.get(url).send())
            .await
            .map_err(|_| UpdateError::Timeout)?
            .map_err(from_reqwest)?;
        let status = resp.status();
        if !status.is_success() {
            return Err(UpdateError::Status(status.as_u16()));
        }
        if resp.content_length().is_some_and(|n| n != size) {
            return Err(UpdateError::SizeMismatch);
        }
        let _ = tokio::fs::remove_file(part).await;
        let mut file = tokio::fs::File::create(part)
            .await
            .map_err(|_| UpdateError::Io)?;
        let mut ctx = Context::new(&SHA256);
        let mut received = 0_u64;
        loop {
            let next = tokio::time::timeout(self.stall, resp.chunk())
                .await
                .map_err(|_| UpdateError::Timeout)?
                .map_err(from_reqwest)?;
            let Some(chunk) = next else { break };
            received += chunk.len() as u64;
            if received > size {
                return Err(UpdateError::SizeMismatch);
            }
            ctx.update(&chunk);
            file.write_all(&chunk).await.map_err(|_| UpdateError::Io)?;
            progress(received, size);
        }
        if received != size {
            return Err(UpdateError::SizeMismatch);
        }
        file.flush().await.map_err(|_| UpdateError::Io)?;
        file.sync_all().await.map_err(|_| UpdateError::Io)?;
        if hex(ctx.finish().as_ref()) != entry.sha256 {
            return Err(UpdateError::HashMismatch);
        }
        Ok(())
    }

    /// The URL when it is a release address for `name`, else `UntrustedUrl`.
    fn trusted_url(
        &self,
        raw: &str,
        version: &semver::Version,
        name: &str,
    ) -> Result<Url, UpdateError> {
        let url = Url::parse(raw).map_err(|_| UpdateError::UntrustedUrl)?;
        if !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(UpdateError::UntrustedUrl);
        }
        let segments: Vec<&str> = url
            .path_segments()
            .ok_or(UpdateError::UntrustedUrl)?
            .collect();
        if segments.last() != Some(&name) {
            return Err(UpdateError::UntrustedUrl);
        }
        let tag = format!("v{version}");
        let github = url.scheme() == "https"
            && url.host_str() == Some("github.com")
            && url.port().is_none()
            && segments
                == [
                    "alwatroos",
                    "sqler",
                    "releases",
                    "download",
                    tag.as_str(),
                    name,
                ];
        let feed_origin = self.loopback && url.origin() == self.feed.origin();
        if github || feed_origin {
            Ok(url)
        } else {
            Err(UpdateError::UntrustedUrl)
        }
    }

    /// Sets how long a download may receive no data before it fails with
    /// [`UpdateError::Timeout`] (default [`crate::DOWNLOAD_STALL_TIMEOUT`]).
    #[must_use]
    pub fn with_stall_timeout(mut self, stall: Duration) -> Self {
        self.stall = stall;
        self
    }
}

/// Removes every entry of `dir` except `keep`.
fn clear_others(dir: &Path, keep: &str) -> Result<(), UpdateError> {
    for entry in std::fs::read_dir(dir)
        .map_err(|_| UpdateError::Io)?
        .flatten()
    {
        if entry.file_name() == keep {
            continue;
        }
        let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
        let _ = if is_dir {
            std::fs::remove_dir_all(entry.path())
        } else {
            std::fs::remove_file(entry.path())
        };
    }
    Ok(())
}
