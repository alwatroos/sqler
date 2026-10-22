// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The WASI context of one plugin: nothing but what the grants allow.
//!
//! No environment, no arguments, no stdin, no sockets (no TCP, UDP or name lookup, and the address check denies
//! everything), no inherited host stdio. Stdout and stderr become log lines. Read-only preopens exist only for the
//! approved `fs.read` scopes. Clocks and random stay linked on purpose (ADR-0055): no capability grants them and the
//! Rust standard library needs them; a clock wait is bounded by the call deadline.

use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use sqler_plugin_api::PluginId;
use tokio::io::AsyncWrite;
use wasmtime_wasi::cli::{IsTerminal, StdoutStream};
use wasmtime_wasi::{FsPerms, WasiCtx, WasiCtxBuilder};

use super::log_host::{SharedLimiter, write_limited};
use crate::log::{LOG_MAX_BYTES, LogLevel};
use crate::scope::{Preopen, ScopeIssue};

/// Builds the context. Blocking (opens the preopened directories): call it off the async workers.
pub(crate) fn build(
    plugin: &PluginId,
    preopens: &[Preopen],
    issues: &[ScopeIssue],
    log: &SharedLimiter,
) -> WasiCtx {
    for issue in issues {
        tracing::warn!(plugin = %plugin, %issue, "fs.read scope has no preopen");
    }
    let mut b = WasiCtxBuilder::new();
    b.allow_tcp(false)
        .allow_udp(false)
        .allow_ip_name_lookup(false)
        .socket_addr_check(|_, _| Box::pin(async { false }))
        .stdout(LineSink::new(plugin, LogLevel::Info, log))
        .stderr(LineSink::new(plugin, LogLevel::Warn, log));
    for p in preopens {
        if let Err(e) = b.preopened_dir(&p.host, &p.guest, FsPerms::ReadOnly) {
            tracing::warn!(plugin = %plugin, scope = %p.scope, error = %e, "cannot preopen a granted fs.read scope");
        }
    }
    b.build()
}

struct SinkInner {
    plugin: PluginId,
    level: LogLevel,
    limiter: SharedLimiter,
    buf: Vec<u8>,
}

impl SinkInner {
    fn line(&self, bytes: &[u8]) {
        let text = String::from_utf8_lossy(bytes);
        write_limited(
            &self.limiter,
            &self.plugin,
            self.level,
            text.trim_end_matches('\r'),
        );
    }

    /// One pass over `data`: each byte is looked at once and copied at most once, whatever the number of newlines.
    /// This is host code the epoch deadline cannot interrupt, so its cost must stay linear in the write.
    fn push(&mut self, data: &[u8]) {
        let mut rest = data;
        while let Some(pos) = rest.iter().position(|b| *b == b'\n') {
            let (head, tail) = rest.split_at(pos);
            self.append(head);
            self.line(&self.buf);
            self.buf.clear();
            rest = &tail[1..];
        }
        self.append(rest);
    }

    /// Adds a newline-free chunk. A line that reaches [`LOG_MAX_BYTES`] is emitted when more of it follows, so a guest
    /// that never writes a newline cannot grow the buffer without bound, while a line of exactly the cap stays one.
    fn append(&mut self, mut chunk: &[u8]) {
        while !chunk.is_empty() {
            if self.buf.len() >= LOG_MAX_BYTES {
                self.line(&self.buf);
                self.buf.clear();
            }
            let take = chunk.len().min(LOG_MAX_BYTES - self.buf.len());
            self.buf.extend_from_slice(&chunk[..take]);
            chunk = &chunk[take..];
        }
    }

    fn flush_partial(&mut self) {
        if !self.buf.is_empty() {
            let b = std::mem::take(&mut self.buf);
            self.line(&b);
        }
    }
}

/// A stdout or stderr that turns lines into log events. All handles the guest opens share one buffer.
#[derive(Clone)]
struct LineSink(Arc<Mutex<SinkInner>>);

impl LineSink {
    fn new(plugin: &PluginId, level: LogLevel, limiter: &SharedLimiter) -> Self {
        Self(Arc::new(Mutex::new(SinkInner {
            plugin: plugin.clone(),
            level,
            limiter: Arc::clone(limiter),
            buf: Vec::new(),
        })))
    }
}

impl IsTerminal for LineSink {
    fn is_terminal(&self) -> bool {
        false
    }
}

impl StdoutStream for LineSink {
    fn async_stream(&self) -> Box<dyn AsyncWrite + Send + Sync> {
        Box::new(self.clone())
    }
}

impl AsyncWrite for LineSink {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(buf);
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .flush_partial();
        Poll::Ready(Ok(()))
    }
}

impl Drop for SinkInner {
    fn drop(&mut self) {
        self.flush_partial();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::LogLimiter;

    fn sink() -> (LineSink, PluginId) {
        let id: PluginId = "demo".parse().unwrap();
        let lim: SharedLimiter = Arc::new(Mutex::new(LogLimiter::new()));
        (LineSink::new(&id, LogLevel::Info, &lim), id)
    }

    #[test]
    fn lines_split_across_writes_and_the_tail_is_kept() {
        let (s, _) = sink();
        let mut inner = s.0.lock().unwrap();
        inner.push(b"ab");
        inner.push(b"c\nde");
        assert_eq!(inner.buf, b"de");
        inner.push(b"\n\nx");
        assert_eq!(inner.buf, b"x");
    }

    #[test]
    fn a_line_without_newline_is_cut_at_the_cap() {
        let (s, _) = sink();
        let mut inner = s.0.lock().unwrap();
        inner.push(&vec![b'a'; LOG_MAX_BYTES + 10]);
        assert_eq!(inner.buf.len(), 10);
        inner.push(&vec![b'a'; LOG_MAX_BYTES - 10]);
        assert_eq!(inner.buf.len(), LOG_MAX_BYTES);
        inner.push(b"z");
        assert_eq!(inner.buf, b"z");
    }

    #[test]
    fn a_line_of_exactly_the_cap_is_one_line() {
        let (s, _) = sink();
        let mut inner = s.0.lock().unwrap();
        inner.push(&vec![b'a'; LOG_MAX_BYTES]);
        inner.push(b"\n");
        assert_eq!(inner.buf, b"");
    }

    #[test]
    fn a_write_of_a_million_newlines_is_linear() {
        let (s, _) = sink();
        let mut inner = s.0.lock().unwrap();
        let data = vec![b'\n'; 1_000_000];
        let t = std::time::Instant::now();
        inner.push(&data);
        // Quadratic copying would be 5e11 bytes (minutes); linear is milliseconds even unoptimised.
        assert!(
            t.elapsed() < std::time::Duration::from_secs(10),
            "{:?}",
            t.elapsed()
        );
        assert_eq!(inner.buf, b"");
        // Short lines followed by a tail keep exactly the tail.
        let mut text = b"ab\n".repeat(300_000);
        text.extend_from_slice(b"tail");
        inner.push(&text);
        assert_eq!(inner.buf, b"tail");
    }
}
