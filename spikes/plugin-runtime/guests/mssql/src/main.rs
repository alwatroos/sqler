// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! SQL Server feasibility guest (`wasi:cli/command`): TLS (CA from a preopened dir) + `select 1`
//! through tiberius over a blocking std `TcpStream`, plus the negative network check.
//!
//! Env: `SPIKE_MSSQL_HOST` (IP), `SPIKE_MSSQL_PORT`, `SPIKE_MSSQL_USER`, `SPIKE_MSSQL_PASSWORD`,
//! `SPIKE_MSSQL_CA` (path inside the guest). The certificate is for `localhost` only, so the TLS name
//! is `localhost` while the socket goes to the IP.

use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::pin::Pin;
use std::process::ExitCode;
use std::task::{Context, Poll};
use std::time::Duration;

use futures::executor::block_on;
use futures::io::{AsyncRead, AsyncWrite};
use tiberius::{AuthMethod, Client, Config, EncryptionLevel};

/// A `TcpStream` behind the `futures-io` traits. Writes block (they complete at once). Reads use a
/// 1 ms read timeout and report `Pending` when nothing arrived: tokio-rustls finishes its handshake
/// by polling once more and relies on `Pending` instead of waiting for data that never comes.
struct Blocking(TcpStream);

impl Blocking {
    fn new(tcp: TcpStream) -> io::Result<Self> {
        tcp.set_read_timeout(Some(Duration::from_millis(1)))?;
        Ok(Self(tcp))
    }
}

impl AsyncRead for Blocking {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut().0.read(buf) {
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                cx.waker().wake_by_ref();
                Poll::Pending
            }
            other => Poll::Ready(other),
        }
    }
}

impl AsyncWrite for Blocking {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Poll::Ready(self.get_mut().0.write(buf))
    }
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(self.get_mut().0.flush())
    }
    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(self.get_mut().0.shutdown(Shutdown::Both))
    }
}

fn env(key: &str) -> Result<String, String> {
    std::env::var(key).map_err(|_| format!("missing env {key}"))
}

fn select1(host: &str, port: u16, user: &str, password: &str, ca: &str) -> Result<i32, String> {
    let tcp = TcpStream::connect((host, port)).map_err(|e| format!("tcp connect: {e}"))?;
    let mut config = Config::new();
    config.host("localhost");
    config.port(port);
    // futures-timer needs a helper thread, which wasm32-wasip2 does not have: no library timeouts.
    config.handshake_timeout(None);
    config.command_timeout(None);
    config.authentication(AuthMethod::sql_server(user, password));
    #[cfg(feature = "tls")]
    {
        config.encryption(EncryptionLevel::Required);
        config.trust_cert_ca(ca);
    }
    #[cfg(not(feature = "tls"))]
    {
        let _ = ca;
        config.encryption(EncryptionLevel::NotSupported);
    }
    block_on(async {
        let mut client = Box::pin(Client::connect(
            config,
            Blocking::new(tcp).map_err(|e| format!("socket setup: {e}"))?,
        ))
        .await
        .map_err(|e| format!("tds connect: {e}"))?;
        let row = client
            .query("select 1", &[])
            .await
            .map_err(|e| format!("query: {e}"))?
            .into_row()
            .await
            .map_err(|e| format!("row: {e}"))?
            .ok_or_else(|| "no row".to_owned())?;
        row.get::<i32, _>(0).ok_or_else(|| "null".to_owned())
    })
}

fn main() -> ExitCode {
    #[cfg(feature = "tls")]
    if rustls::crypto::ring::default_provider()
        .install_default()
        .is_err()
    {
        eprintln!("a default crypto provider was already installed");
    }
    let run = || -> Result<(String, u16, String, String, String), String> {
        Ok((
            env("SPIKE_MSSQL_HOST")?,
            env("SPIKE_MSSQL_PORT")?
                .parse::<u16>()
                .map_err(|e| e.to_string())?,
            env("SPIKE_MSSQL_USER")?,
            env("SPIKE_MSSQL_PASSWORD")?,
            env("SPIKE_MSSQL_CA")?,
        ))
    };
    let (host, port, user, password, ca) = match run() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    // Negative check first: a port that `--allow-tcp` does not list must be refused by the host.
    let denied = TcpStream::connect((host.as_str(), port.wrapping_add(1)));
    let denied_ok = match &denied {
        Err(e) => {
            eprintln!("denied connect: kind={:?} msg={e}", e.kind());
            e.kind() == io::ErrorKind::PermissionDenied
        }
        Ok(_) => false,
    };
    println!(
        "SPIKE guest_mssql_tls={}",
        if cfg!(feature = "tls") {
            "verified"
        } else {
            "none"
        }
    );
    println!("SPIKE guest_mssql_denied_ok={}", u8::from(denied_ok));
    match select1(&host, port, &user, &password, &ca) {
        Ok(v) => {
            println!("SPIKE guest_mssql_select1={v}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            println!("SPIKE guest_mssql_select1=fail");
            eprintln!("mssql error: {e}");
            ExitCode::FAILURE
        }
    }
}
