// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The stdio transport: newline-delimited JSON-RPC over any reader and writer.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, ReadBuf};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::error::McpError;
use crate::protocol::{
    Engine, INTERNAL_ERROR, Message, PARSE_ERROR, Session, classify, error_response,
};
use crate::server::McpServer;

/// How many `tools/call` requests one session may have pending (running or waiting for a permit).
const MAX_PENDING: usize = 64;

/// A reader that fails once a line grows past a cap without a newline.
pub(crate) struct CappedLines<R> {
    inner: R,
    cap: usize,
    line: usize,
    tripped: Arc<AtomicBool>,
}

impl<R> CappedLines<R> {
    pub(crate) fn new(inner: R, cap: usize, tripped: Arc<AtomicBool>) -> Self {
        Self {
            inner,
            cap,
            line: 0,
            tripped,
        }
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for CappedLines<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let polled = Pin::new(&mut self.inner).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = &polled {
            let mut line = self.line;
            let mut over = false;
            for &byte in &buf.filled()[before..] {
                if byte == b'\n' {
                    line = 0;
                } else {
                    line += 1;
                    if line > self.cap {
                        over = true;
                        break;
                    }
                }
            }
            self.line = line;
            if over {
                buf.set_filled(before);
                self.tripped.store(true, Ordering::SeqCst);
                return Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "request line over the size cap",
                )));
            }
        }
        polled
    }
}

impl McpServer {
    /// Serves one session over `read` and `write` (newline-delimited JSON-RPC) until the reader
    /// ends, `shutdown` resolves, or a line exceeds `max_request_bytes`.
    ///
    /// At end of input the calls still running finish and are answered; on `shutdown` they are
    /// cancelled.
    ///
    /// # Errors
    ///
    /// `McpError::RequestTooLarge` for an over-cap line, `McpError::Transport` when the output
    /// fails. End of input and `shutdown` are clean (`Ok`).
    pub async fn serve_io<R, W>(
        self,
        read: R,
        write: W,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) -> Result<(), McpError>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let tripped = Arc::new(AtomicBool::new(false));
        let reader = CappedLines::new(read, self.shared.options.max_request_bytes, tripped.clone());
        let mut lines = BufReader::new(reader).lines();
        let engine = Engine::new(self.shared);
        let session = Arc::new(Session::default());
        let stop = CancellationToken::new();
        let (out, rx) = mpsc::channel::<String>(256);
        let mut writer = tokio::spawn(write_lines(write, rx));
        let mut calls = JoinSet::new();
        tokio::pin!(shutdown);

        let mut shut = false;
        let mut read_error: Option<io::Error> = None;
        let read_failed = loop {
            let line = tokio::select! {
                () = &mut shutdown => { shut = true; break false; }
                line = lines.next_line() => line,
            };
            let line = match line {
                Ok(Some(line)) => line,
                Ok(None) => break false,
                Err(err) => {
                    read_error = Some(err);
                    break true;
                }
            };
            if line.trim().is_empty() {
                continue;
            }
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                let reply = error_response(&Value::Null, PARSE_ERROR, "parse error");
                let _ = out.send(reply.to_string()).await;
                continue;
            };
            match classify(value) {
                Message::Invalid(reply) => {
                    let _ = out.send(reply.to_string()).await;
                }
                Message::Response => {}
                Message::Notification { method, params } => {
                    Engine::notify(&session, &method, params.as_ref());
                }
                Message::Request { id, method, params } if method == "tools/call" => {
                    if calls.len() >= MAX_PENDING {
                        let reply =
                            error_response(&id, INTERNAL_ERROR, "too many pending requests");
                        let _ = out.send(reply.to_string()).await;
                        continue;
                    }
                    let (engine, session, stop, out) =
                        (engine.clone(), session.clone(), stop.clone(), out.clone());
                    calls.spawn(async move {
                        if let Some(reply) = engine
                            .call_tool(&session, &id, params.as_ref(), &stop)
                            .await
                        {
                            let _ = out.send(reply.to_string()).await;
                        }
                    });
                }
                Message::Request { id, method, params } => {
                    let reply = engine.quick(&session, &id, &method, params.as_ref());
                    let _ = out.send(reply.to_string()).await;
                }
            }
            // Reap finished calls so the pending count stays honest.
            while calls.try_join_next().is_some() {}
        };

        if shut || read_failed {
            stop.cancel();
            session.cancel_all();
            calls.abort_all();
        }
        // At end of input the running calls finish; `shutdown` still cuts them short.
        loop {
            tokio::select! {
                joined = calls.join_next() => if joined.is_none() { break },
                () = &mut shutdown, if !shut => {
                    stop.cancel();
                    session.cancel_all();
                    shut = true;
                }
            }
        }
        drop(out);
        let written = (&mut writer).await;
        if tripped.load(Ordering::SeqCst) {
            return Err(McpError::RequestTooLarge);
        }
        if let Some(err) = read_error {
            return Err(McpError::Transport(err.to_string()));
        }
        match written {
            Ok(Ok(())) => Ok(()),
            Ok(Err(err)) => Err(McpError::Transport(err.to_string())),
            Err(err) => Err(McpError::Transport(err.to_string())),
        }
    }

    /// Serves one session over the process's stdin and stdout.
    ///
    /// # Errors
    ///
    /// As [`McpServer::serve_io`].
    pub async fn serve_stdio(
        self,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) -> Result<(), McpError> {
        self.serve_io(tokio::io::stdin(), tokio::io::stdout(), shutdown)
            .await
    }
}

/// Writes each message as one line and flushes it; ends when every sender is gone.
async fn write_lines<W: AsyncWrite + Unpin>(
    mut write: W,
    mut rx: mpsc::Receiver<String>,
) -> io::Result<()> {
    while let Some(line) = rx.recv().await {
        write.write_all(line.as_bytes()).await?;
        write.write_all(b"\n").await?;
        write.flush().await?;
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    async fn read_all(data: &[u8], cap: usize) -> (io::Result<Vec<u8>>, bool) {
        let tripped = Arc::new(AtomicBool::new(false));
        let mut reader = CappedLines::new(data, cap, tripped.clone());
        let mut out = Vec::new();
        let result = reader.read_to_end(&mut out).await.map(|_| out);
        (result, tripped.load(Ordering::SeqCst))
    }

    #[tokio::test]
    async fn lines_within_the_cap_pass() {
        let (out, tripped) = read_all(b"aaaa\nbbbb\ncccc", 4).await;
        assert_eq!(out.unwrap(), b"aaaa\nbbbb\ncccc");
        assert!(!tripped);
    }

    #[tokio::test]
    async fn a_long_line_trips() {
        let (out, tripped) = read_all(b"aaaa\nbbbbb\n", 4).await;
        assert!(out.is_err());
        assert!(tripped);
    }

    #[tokio::test]
    async fn the_count_spans_reads() {
        let (mut tx, rx) = tokio::io::duplex(8);
        let tripped = Arc::new(AtomicBool::new(false));
        let mut reader = CappedLines::new(rx, 6, tripped.clone());
        tokio::io::AsyncWriteExt::write_all(&mut tx, b"abcd")
            .await
            .unwrap();
        let mut buf = [0_u8; 4];
        reader.read_exact(&mut buf).await.unwrap();
        tokio::io::AsyncWriteExt::write_all(&mut tx, b"efg")
            .await
            .unwrap();
        assert!(reader.read(&mut buf).await.is_err());
        assert!(tripped.load(Ordering::SeqCst));
    }
}
