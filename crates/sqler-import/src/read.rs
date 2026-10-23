// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Bounded reading of a foreign file.

use std::io::Read;
use std::path::Path;

use crate::MAX_FILE_BYTES;
use crate::error::ImportError;

fn io(e: &std::io::Error) -> ImportError {
    ImportError::Io { kind: e.kind() }
}

/// Reads a regular file of at most [`MAX_FILE_BYTES`].
///
/// # Errors
/// [`ImportError::Io`], [`ImportError::NotAFile`] or [`ImportError::TooLarge`]. A file that grows
/// while it is read is cut at the limit and reported as too large.
pub fn read_source(path: &Path) -> Result<Vec<u8>, ImportError> {
    // The checks on the path come first so that opening a pipe or a device never blocks.
    std::fs::symlink_metadata(path).map_err(|e| io(&e))?;
    let meta = std::fs::metadata(path).map_err(|e| io(&e))?;
    if !meta.is_file() {
        return Err(ImportError::NotAFile);
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(ImportError::TooLarge {
            limit: MAX_FILE_BYTES,
        });
    }
    let file = std::fs::File::open(path).map_err(|e| io(&e))?;
    if !file.metadata().map_err(|e| io(&e))?.is_file() {
        return Err(ImportError::NotAFile);
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| io(&e))?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(ImportError::TooLarge {
            limit: MAX_FILE_BYTES,
        });
    }
    Ok(bytes)
}
