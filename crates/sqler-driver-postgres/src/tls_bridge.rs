// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! A per-session local bridge from a Unix socket to the dial `host:port`.
//!
//! `sqlx` runs TLS against the connect host and also dials it. Through an SSH tunnel the dial
//! address is `127.0.0.1:<port>` while the certificate names the original host. When the socket
//! path is set, `sqlx` dials the socket and still verifies against the host, so the bridge
//! separates the two. Verification is never weakened; only the transport is redirected.

use std::io;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sqler_driver_api::DriverError;
use tokio::io::copy_bidirectional;
use tokio::net::{TcpStream, UnixListener};
use tokio::task::{JoinHandle, JoinSet};

/// Distinguishes the directories of bridges opened by one process.
static COUNTER: AtomicU64 = AtomicU64::new(0);

/// The pause after a failed `accept`, so a persistent error does not spin.
const ACCEPT_RETRY: std::time::Duration = std::time::Duration::from_millis(50);

/// How often a taken directory name is retried.
const DIR_TRIES: u8 = 8;

/// A listening Unix socket in a private (`0700`) directory that forwards to `host:port`.
///
/// Dropping it stops the accept loop (which aborts the running forwards) and removes the socket
/// and the directory.
pub(crate) struct TlsBridge {
    dir: PathBuf,
    socket: PathBuf,
    task: JoinHandle<()>,
}

fn error(e: &io::Error) -> DriverError {
    DriverError::Connection(format!("could not start the local TLS bridge: {e}"))
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

/// Creates a fresh directory owned by the user only.
fn make_dir() -> io::Result<PathBuf> {
    let mut last = None;
    for _ in 0..DIR_TRIES {
        let dir = base_dir().join(format!("sqler-tls-{:016x}", nonce()));
        match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => last = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| io::Error::from(io::ErrorKind::AlreadyExists)))
}

impl TlsBridge {
    /// Starts a bridge to `dial_host:dial_port`; `file_name` is the socket file inside the
    /// directory. Must run inside a Tokio runtime.
    pub(crate) fn open(
        dial_host: &str,
        dial_port: u16,
        file_name: &str,
    ) -> Result<Self, DriverError> {
        let dir = make_dir().map_err(|e| error(&e))?;
        let socket = dir.join(file_name);
        let listener = match UnixListener::bind(&socket) {
            Ok(listener) => listener,
            Err(e) => {
                let _ = std::fs::remove_dir(&dir);
                return Err(error(&e));
            }
        };
        let host = dial_host.to_owned();
        let task = tokio::spawn(async move {
            let mut forwards = JoinSet::new();
            loop {
                // An accept error (descriptor exhaustion, an aborted connection) is transient:
                // only dropping the bridge ends the loop.
                let Ok((mut inbound, _)) = listener.accept().await else {
                    tokio::time::sleep(ACCEPT_RETRY).await;
                    continue;
                };
                let host = host.clone();
                forwards.spawn(async move {
                    // A failed dial drops `inbound`, which the client sees as a closed stream.
                    if let Ok(mut outbound) = TcpStream::connect((host.as_str(), dial_port)).await {
                        let _ = copy_bidirectional(&mut inbound, &mut outbound).await;
                    }
                });
                // Reap finished forwards so a long session does not collect them.
                while forwards.try_join_next().is_some() {}
            }
        });
        Ok(Self { dir, socket, task })
    }

    /// The directory (`PostgreSQL` takes the directory, not the socket file).
    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    #[cfg(test)]
    pub(crate) fn socket_path(&self) -> &Path {
        &self.socket
    }
}

impl Drop for TlsBridge {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_file(&self.socket);
        let _ = std::fs::remove_dir(&self.dir);
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::{TcpListener, UnixStream};

    use super::*;

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    #[test]
    fn round_trip_through_a_tcp_echo_server() {
        rt().block_on(async {
            let echo = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = echo.local_addr().unwrap().port();
            tokio::spawn(async move {
                let (mut s, _) = echo.accept().await.unwrap();
                let mut buf = [0_u8; 5];
                s.read_exact(&mut buf).await.unwrap();
                s.write_all(&buf).await.unwrap();
            });
            let bridge = TlsBridge::open("127.0.0.1", port, ".s.PGSQL.1").unwrap();
            let mut client = UnixStream::connect(bridge.socket_path()).await.unwrap();
            client.write_all(b"hello").await.unwrap();
            let mut buf = [0_u8; 5];
            client.read_exact(&mut buf).await.unwrap();
            assert_eq!(&buf, b"hello");
        });
    }

    #[test]
    fn dir_is_private_and_removed_on_drop() {
        rt().block_on(async {
            let bridge = TlsBridge::open("127.0.0.1", 1, ".s.PGSQL.1").unwrap();
            let dir = bridge.dir().to_owned();
            let socket = bridge.socket_path().to_owned();
            let mode = std::fs::metadata(&dir).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700);
            assert!(socket.exists());
            drop(bridge);
            assert!(!socket.exists());
            assert!(!dir.exists());
        });
    }

    #[test]
    fn bridges_get_distinct_dirs() {
        rt().block_on(async {
            let a = TlsBridge::open("127.0.0.1", 1, "a").unwrap();
            let b = TlsBridge::open("127.0.0.1", 1, "a").unwrap();
            assert_ne!(a.dir(), b.dir());
        });
    }

    #[test]
    fn a_failed_dial_closes_the_client_stream() {
        rt().block_on(async {
            // Port 1 on loopback refuses.
            let bridge = TlsBridge::open("127.0.0.1", 1, "a").unwrap();
            let mut client = UnixStream::connect(bridge.socket_path()).await.unwrap();
            let mut buf = [0_u8; 1];
            assert_eq!(client.read(&mut buf).await.unwrap_or(0), 0);
        });
    }
}
