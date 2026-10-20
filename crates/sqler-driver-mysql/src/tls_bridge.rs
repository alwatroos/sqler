// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! A per-session local bridge from a Unix socket to the dial address.
//!
//! `sqlx` dials `options.host` and also verifies the server certificate (and sends SNI) against
//! it. With a Unix socket set, it dials the socket and still runs TLS against `host`. The bridge
//! is that socket: it forwards every accepted stream to the dial address (an SSH tunnel's
//! `127.0.0.1:<port>`), so the driver can set `host` to the original database host. The
//! certificate checks stay `sqlx`'s own (ADR-0019).
//!
//! The socket lives in a fresh directory with mode `0700`, so only the owner can reach it. Both
//! are removed on drop.

use std::fs;
use std::io;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sqler_driver_api::DriverError;
use tokio::net::{TcpStream, UnixListener};
use tokio::task::{JoinHandle, JoinSet};

/// Distinguishes the bridge directories of one process.
static COUNTER: AtomicU64 = AtomicU64::new(0);

/// The pause after a failed `accept`, so a persistent error does not spin.
const ACCEPT_RETRY: std::time::Duration = std::time::Duration::from_millis(50);

/// How many directory names are tried before giving up.
const MAX_TRIES: usize = 8;

/// The running bridge. Dropping it stops the forwards and removes the socket and its directory.
pub(crate) struct TlsBridge {
    dir: PathBuf,
    socket: PathBuf,
    task: JoinHandle<()>,
}

fn start_error(err: &io::Error) -> DriverError {
    DriverError::Connection(format!("could not start the local TLS bridge: {err}"))
}

/// An unpredictable 64-bit nonce for the directory name.
///
/// `RandomState` keys come from the operating system's random source; they are mixed with the
/// process id, the clock and a counter so that two bridges never share a name.
fn nonce() -> u64 {
    use std::hash::{BuildHasher as _, Hasher as _, RandomState};
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u32(std::process::id());
    hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    if let Ok(since) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        hasher.write_u128(since.as_nanos());
    }
    hasher.finish()
}

/// The parent of the bridge directories: `$XDG_RUNTIME_DIR` when it is an absolute directory
/// (private to the user, by convention), else the temp directory.
fn base_dir() -> PathBuf {
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") {
        let runtime = PathBuf::from(runtime);
        if runtime.is_absolute() && runtime.is_dir() {
            return runtime;
        }
    }
    std::env::temp_dir()
}

/// Creates the `0700` directory, retrying on a name clash.
fn make_dir() -> io::Result<PathBuf> {
    let mut last = io::Error::from(io::ErrorKind::AlreadyExists);
    for _ in 0..MAX_TRIES {
        let dir = base_dir().join(format!("sqler-tls-{:016x}", nonce()));
        match fs::DirBuilder::new().mode(0o700).create(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => last = e,
            Err(e) => return Err(e),
        }
    }
    Err(last)
}

async fn accept_loop(listener: UnixListener, host: String, port: u16) {
    let mut forwards = JoinSet::new();
    loop {
        // An accept error (descriptor exhaustion, an aborted connection) is transient: only
        // dropping the bridge ends the loop.
        let Ok((mut local, _)) = listener.accept().await else {
            tokio::time::sleep(ACCEPT_RETRY).await;
            continue;
        };
        // Reap the forwards that already ended.
        while forwards.try_join_next().is_some() {}
        let host = host.clone();
        forwards.spawn(async move {
            // A failed dial drops the local stream, which the client sees as a closed
            // connection.
            if let Ok(mut remote) = TcpStream::connect((host.as_str(), port)).await {
                let _ = tokio::io::copy_bidirectional(&mut local, &mut remote).await;
            }
        });
    }
}

impl TlsBridge {
    /// Starts a bridge that forwards to `dial_host:dial_port`, with the socket named
    /// `file_name`. Must run inside a Tokio runtime.
    pub(crate) fn open(
        dial_host: &str,
        dial_port: u16,
        file_name: &str,
    ) -> Result<Self, DriverError> {
        let dir = make_dir().map_err(|e| start_error(&e))?;
        let socket = dir.join(file_name);
        let listener = match UnixListener::bind(&socket) {
            Ok(listener) => listener,
            Err(e) => {
                let _ = fs::remove_dir(&dir);
                return Err(start_error(&e));
            }
        };
        let task = tokio::spawn(accept_loop(listener, dial_host.to_owned(), dial_port));
        Ok(Self { dir, socket, task })
    }

    /// The directory that holds the socket.
    #[allow(dead_code)]
    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    /// The socket path.
    pub(crate) fn socket_path(&self) -> &Path {
        &self.socket
    }
}

impl Drop for TlsBridge {
    fn drop(&mut self) {
        self.task.abort();
        let _ = fs::remove_file(&self.socket);
        let _ = fs::remove_dir(&self.dir);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::{TcpListener, UnixStream};

    use super::*;

    #[tokio::test]
    async fn forwards_a_stream_to_the_dial_address() {
        let echo = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = echo.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut s, _) = echo.accept().await.unwrap();
            let mut buf = [0_u8; 5];
            s.read_exact(&mut buf).await.unwrap();
            s.write_all(&buf).await.unwrap();
        });
        let bridge = TlsBridge::open("127.0.0.1", port, "mysql.sock").unwrap();
        let mut c = UnixStream::connect(bridge.socket_path()).await.unwrap();
        c.write_all(b"hello").await.unwrap();
        let mut got = [0_u8; 5];
        c.read_exact(&mut got).await.unwrap();
        assert_eq!(&got, b"hello");
    }

    #[tokio::test]
    async fn dir_is_private_and_removed_on_drop() {
        let bridge = TlsBridge::open("127.0.0.1", 1, "mysql.sock").unwrap();
        let dir = bridge.dir().to_owned();
        let socket = bridge.socket_path().to_owned();
        let mode = fs::metadata(&dir).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
        assert!(socket.exists());
        drop(bridge);
        assert!(!socket.exists());
        assert!(!dir.exists());
    }

    #[tokio::test]
    async fn bridges_get_distinct_dirs() {
        let a = TlsBridge::open("127.0.0.1", 1, "mysql.sock").unwrap();
        let b = TlsBridge::open("127.0.0.1", 1, "mysql.sock").unwrap();
        assert_ne!(a.dir(), b.dir());
    }

    #[tokio::test]
    async fn a_failed_dial_closes_the_client_stream() {
        // Port 1 on loopback refuses the connection.
        let bridge = TlsBridge::open("127.0.0.1", 1, "mysql.sock").unwrap();
        let mut c = UnixStream::connect(bridge.socket_path()).await.unwrap();
        let mut buf = [0_u8; 1];
        assert_eq!(c.read(&mut buf).await.unwrap_or(0), 0);
    }
}
