// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `OllamaProvider`: the `AiProvider` implementation.

use std::net::IpAddr;
use std::time::Duration;

use reqwest::header::CONTENT_TYPE;
use reqwest::redirect::Policy;
use reqwest::{Client, Url};
use serde::de::DeserializeOwned;
use sqler_ai_api::{
    AiError, AiProvider, BoxFuture, Capabilities, ChatRequest, ChatStream, ModelInfo,
};
use tokio::task::JoinSet;

use crate::convert::{capabilities_from_show, context_length, request_body};
use crate::error::{from_reqwest, from_response, malformed, read_capped};
use crate::stream::OllamaStream;
use crate::wire::{Show, ShowBody, Tags};

/// The endpoint of a default Ollama install.
pub const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:11434";

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a response may stay silent before the request fails. A model that
/// is still loading from a slow disk can exceed it; see `with_read_timeout`.
const READ_TIMEOUT: Duration = Duration::from_secs(120);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// How many `/api/show` calls `list_models` keeps in flight.
const SHOW_CONCURRENCY: usize = 4;
/// The time `list_models` spends on all `/api/show` calls together.
const SHOW_DEADLINE: Duration = Duration::from_secs(30);
const MAX_JSON_BODY: usize = 16 * 1024 * 1024;

/// An `AiProvider` for an Ollama server, over its native API.
///
/// # Tokio
///
/// The futures and streams of this type must be polled inside a Tokio runtime
/// with I/O and time enabled.
///
/// # Locality
///
/// [`is_local`](Self::is_local) is decided by the endpoint host: `localhost`
/// or a loopback address is local, anything else is a remote server.
///
/// # Capabilities
///
/// [`AiProvider::capabilities`] reports what the provider can do in general.
/// [`model_capabilities`](Self::model_capabilities) asks the server about one
/// model; when the server does not report a `capabilities` list the defaults
/// are tools on, JSON mode on and vision off.
#[derive(Debug, Clone)]
pub struct OllamaProvider {
    client: Client,
    endpoint: String,
    local: bool,
}

impl OllamaProvider {
    /// Creates a provider. `None` selects [`DEFAULT_ENDPOINT`].
    ///
    /// # Errors
    ///
    /// [`AiError::Invalid`] when the endpoint is not an `http` or `https` URL
    /// with a host, or when it carries a user name, a password, a query or a fragment.
    pub fn new(endpoint: Option<&str>) -> Result<Self, AiError> {
        let raw = endpoint.unwrap_or(DEFAULT_ENDPOINT).trim();
        let url = Url::parse(raw)
            .map_err(|_| AiError::Invalid("the Ollama endpoint is not a valid URL".to_owned()))?;
        if !matches!(url.scheme(), "http" | "https") || url.host().is_none() {
            return Err(AiError::Invalid(
                "the Ollama endpoint must be an http or https URL with a host".to_owned(),
            ));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(AiError::Invalid(
                "the Ollama endpoint must not contain a user name or password".to_owned(),
            ));
        }
        if url.query().is_some() || url.fragment().is_some() {
            return Err(AiError::Invalid(
                "the Ollama endpoint must not contain a query or fragment".to_owned(),
            ));
        }
        let local = host_is_local(&url);
        let client = build_client(local, READ_TIMEOUT)?;
        Ok(Self {
            client,
            endpoint: raw.trim_end_matches('/').to_owned(),
            local,
        })
    }

    /// Sets how long a response may stay silent (no byte received) before the
    /// request fails with `Network("timed out")`. The default is 120 seconds;
    /// raise it when a large model loads slowly.
    ///
    /// # Errors
    ///
    /// [`AiError::Provider`] when the HTTP client cannot be rebuilt.
    pub fn with_read_timeout(mut self, timeout: Duration) -> Result<Self, AiError> {
        self.client = build_client(self.local, timeout)?;
        Ok(self)
    }

    /// The endpoint as configured, without a trailing `/`.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Whether the endpoint host is `localhost` or a loopback address, so no
    /// data leaves the machine.
    #[must_use]
    pub fn is_local(&self) -> bool {
        self.local
    }

    /// What one model supports, from `POST /api/show`.
    ///
    /// `json_mode` is always on. Without a `capabilities` list in the answer,
    /// tools are on and vision is off.
    pub fn model_capabilities(&self, model: &str) -> BoxFuture<'_, Result<Capabilities, AiError>> {
        let model = model.to_owned();
        Box::pin(async move { Ok(capabilities_from_show(&self.show(&model).await?)) })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.endpoint)
    }

    async fn show(&self, model: &str) -> Result<Show, AiError> {
        let body = serde_json::to_vec(&ShowBody { model })
            .map_err(|_| AiError::Invalid("cannot encode the request".to_owned()))?;
        let req = self
            .client
            .post(self.url("/api/show"))
            .header(CONTENT_TYPE, "application/json")
            .body(body);
        self.read_json(req).await
    }

    async fn read_json<T: DeserializeOwned>(
        &self,
        req: reqwest::RequestBuilder,
    ) -> Result<T, AiError> {
        let mut resp = req
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(from_reqwest)?;
        if !resp.status().is_success() {
            return Err(from_response(resp).await);
        }
        let bytes = read_capped(&mut resp, MAX_JSON_BODY).await?;
        serde_json::from_slice(&bytes).map_err(|e| malformed("JSON response", &e))
    }
}

/// Runs the futures with at most `limit` in flight and returns their outputs in
/// input order. A future that has not finished when `deadline` passes, or that
/// panics, yields `None`; unfinished futures are dropped.
async fn run_bounded<I, F, T>(jobs: I, limit: usize, deadline: Duration) -> Vec<Option<T>>
where
    I: IntoIterator<Item = F>,
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let mut pending = jobs.into_iter().enumerate();
    let mut set = JoinSet::new();
    let mut out: Vec<Option<T>> = Vec::new();
    let end = tokio::time::Instant::now() + deadline;
    loop {
        while set.len() < limit.max(1) {
            let Some((i, job)) = pending.next() else {
                break;
            };
            out.push(None);
            set.spawn(async move { (i, job.await) });
        }
        match tokio::time::timeout_at(end, set.join_next()).await {
            Ok(Some(Ok((i, v)))) => out[i] = Some(v),
            Ok(Some(Err(_))) => {}
            Ok(None) | Err(_) => break,
        }
    }
    // Jobs that never started keep `None`.
    out.extend(pending.map(|_| None));
    out
}

fn build_client(local: bool, read_timeout: Duration) -> Result<Client, AiError> {
    let mut builder = Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(read_timeout)
        .redirect(Policy::none());
    if local {
        // A loopback server must not be reached through an environment proxy.
        builder = builder.no_proxy();
    }
    builder
        .build()
        .map_err(|_| AiError::Provider("cannot build the HTTP client".to_owned()))
}

fn host_is_local(url: &Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    // `host_str` keeps the brackets of an IPv6 literal.
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = bare.parse::<IpAddr>() {
        return ip.is_loopback();
    }
    host.eq_ignore_ascii_case("localhost")
}

impl AiProvider for OllamaProvider {
    fn id(&self) -> &'static str {
        "ollama"
    }

    fn list_models(&self) -> BoxFuture<'_, Result<Vec<ModelInfo>, AiError>> {
        Box::pin(async move {
            let tags: Tags = self
                .read_json(self.client.get(self.url("/api/tags")))
                .await?;
            let mut jobs = Vec::with_capacity(tags.models.len());
            for m in &tags.models {
                let provider = self.clone();
                let name = m.name.clone();
                jobs.push(async move {
                    provider
                        .show(&name)
                        .await
                        .ok()
                        .as_ref()
                        .and_then(context_length)
                });
            }
            // A failed, slow or missing `show` only leaves the window unknown.
            let windows = run_bounded(jobs, SHOW_CONCURRENCY, SHOW_DEADLINE).await;
            let models = tags
                .models
                .into_iter()
                .zip(windows)
                .map(|(m, window)| {
                    let info = ModelInfo::new(m.name);
                    match window.flatten() {
                        Some(n) => info.with_context_window(n),
                        None => info,
                    }
                })
                .collect();
            Ok(models)
        })
    }

    fn stream(&self, request: ChatRequest) -> Box<dyn ChatStream + '_> {
        match request_body(&request) {
            Err(e) => Box::new(OllamaStream::failed(e)),
            Ok(body) => {
                let fut = self
                    .client
                    .post(self.url("/api/chat"))
                    .header(CONTENT_TYPE, "application/json")
                    .body(body)
                    .send();
                Box::new(OllamaStream::sending(Box::pin(fut)))
            }
        }
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::new(true, true, true, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(endpoint: &str) -> bool {
        OllamaProvider::new(Some(endpoint)).unwrap().is_local()
    }

    #[test]
    fn locality() {
        assert!(local("http://localhost:11434"));
        assert!(local("http://LOCALHOST:11434"));
        assert!(local("http://127.0.0.2:1"));
        assert!(local("http://[::1]:11434"));
        assert!(!local("http://10.0.0.1:11434"));
        assert!(!local("https://example.com"));
    }

    #[test]
    fn endpoint_default_and_trimmed() {
        let p = OllamaProvider::new(None).unwrap();
        assert_eq!(p.endpoint(), DEFAULT_ENDPOINT);
        assert!(p.is_local());
        assert_eq!(
            OllamaProvider::new(Some("http://h:1/")).unwrap().endpoint(),
            "http://h:1"
        );
        assert_eq!(p.id(), "ollama");
        assert_eq!(p.capabilities(), Capabilities::new(true, true, true, None));
    }

    #[test]
    fn rejects_bad_endpoints() {
        for bad in ["ftp://x", "not a url", "", "http://", "file:///tmp/x"] {
            assert!(
                matches!(OllamaProvider::new(Some(bad)), Err(AiError::Invalid(_))),
                "{bad}"
            );
        }
    }

    #[test]
    fn rejects_query_and_fragment_without_leaking_them() {
        for bad in ["http://h:1/?token=abc", "http://h:1/#zzz", "http://h:1?a=b"] {
            match OllamaProvider::new(Some(bad)) {
                Err(AiError::Invalid(m)) => {
                    assert!(!m.contains("abc") && !m.contains("zzz"), "{m}");
                }
                other => panic!("{bad}: {other:?}"),
            }
        }
    }

    async fn nap(ms: u64, v: u32) -> u32 {
        tokio::time::sleep(Duration::from_millis(ms)).await;
        v
    }

    #[tokio::test]
    async fn run_bounded_keeps_order_and_bounds_concurrency() {
        // Later jobs finish first; the output still follows the input.
        let jobs = (0..6u32).map(|i| nap(u64::from(60 - i * 10), i));
        let out = run_bounded(jobs, 2, Duration::from_secs(10)).await;
        assert_eq!(out, (0..6).map(Some).collect::<Vec<_>>());
    }

    #[tokio::test]
    async fn run_bounded_deadline_leaves_unfinished_as_none() {
        let jobs = vec![nap(1, 0), nap(60_000, 1), nap(1, 2), nap(1, 3)];
        let started = std::time::Instant::now();
        let out = run_bounded(jobs, 2, Duration::from_millis(300)).await;
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(out, vec![Some(0), None, Some(2), Some(3)]);
        // With one slot a stuck job blocks the rest, which never start.
        let jobs = vec![nap(60_000, 0), nap(1, 1)];
        let out = run_bounded(jobs, 1, Duration::from_millis(200)).await;
        assert_eq!(out, vec![None, None]);
    }

    #[test]
    fn is_send_sync() {
        fn check<T: Send + Sync>() {}
        check::<OllamaProvider>();
    }
}
