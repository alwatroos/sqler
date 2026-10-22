// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `AnthropicProvider`: the `AiProvider` implementation.

use std::fmt;
use std::net::IpAddr;
use std::time::Duration;

use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderName, HeaderValue};
use reqwest::redirect::Policy;
use reqwest::{Client, Method, RequestBuilder, Url};
use serde::de::DeserializeOwned;
use sqler_ai_api::{
    AiError, AiProvider, BoxFuture, Capabilities, ChatRequest, ChatStream, ModelInfo,
};

use crate::convert::{model_capabilities_for, request_body};
use crate::error::{from_reqwest, from_response, malformed, read_capped};
use crate::stream::AnthropicStream;
use crate::wire::ModelsPage;

/// The endpoint of the Anthropic API.
pub const DEFAULT_ENDPOINT: &str = "https://api.anthropic.com";
/// The `anthropic-version` header value sent with every request.
pub const API_VERSION: &str = "2023-06-01";
/// The `max_tokens` sent when the request sets no `max_output_tokens`. The API
/// requires the field; 4096 is below the output limit of every model.
pub const DEFAULT_MAX_TOKENS: u32 = 4096;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a response may stay silent before the request fails.
const READ_TIMEOUT: Duration = Duration::from_secs(120);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_JSON_BODY: usize = 16 * 1024 * 1024;
const MODELS_PAGE_SIZE: &str = "1000";
const MAX_MODEL_PAGES: usize = 10;
const KEY_HEADER: HeaderName = HeaderName::from_static("x-api-key");
const VERSION_HEADER: HeaderName = HeaderName::from_static("anthropic-version");

/// An `AiProvider` for the Anthropic Messages API.
///
/// # Tokio
///
/// The futures and streams of this type must be polled inside a Tokio runtime
/// with I/O and time enabled.
///
/// # Remote
///
/// Anthropic is a remote provider: prompts and results leave the machine.
/// [`is_local`](Self::is_local) is always false, even for a loopback endpoint
/// (a test server or a proxy).
///
/// # The API key
///
/// The key is a constructor argument and is kept only as a sensitive HTTP
/// header value. `Debug` prints `"<redacted>"`, and every error text has the
/// key replaced by `***`.
///
/// # Defaults and gaps
///
/// A request without `max_output_tokens` sends [`DEFAULT_MAX_TOKENS`], because
/// the API requires the field. `ResponseFormat::Json` fails with
/// `AiError::Unsupported`: the Messages API has no schema-less JSON mode.
#[derive(Clone)]
pub struct AnthropicProvider {
    client: Client,
    endpoint: String,
    key: HeaderValue,
    loopback: bool,
}

impl fmt::Debug for AnthropicProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AnthropicProvider")
            .field("endpoint", &self.endpoint)
            .field("api_key", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl AnthropicProvider {
    /// Creates a provider. `None` selects [`DEFAULT_ENDPOINT`].
    ///
    /// # Errors
    ///
    /// [`AiError::Invalid`] when the key is empty or is not a valid header
    /// value (the key is never echoed), or when the endpoint is not an `https`
    /// URL with a host. `http` is accepted only for a loopback host, so the
    /// key never travels in clear text. A user name, password, query or
    /// fragment in the URL is refused too.
    pub fn new(api_key: &str, endpoint: Option<&str>) -> Result<Self, AiError> {
        let key = api_key.trim();
        if key.is_empty() {
            return Err(AiError::Invalid(
                "the Anthropic API key is empty".to_owned(),
            ));
        }
        let mut key = if key.is_ascii() {
            HeaderValue::from_str(key).ok()
        } else {
            None
        }
        .ok_or_else(|| {
            AiError::Invalid("the Anthropic API key contains invalid characters".to_owned())
        })?;
        key.set_sensitive(true);

        let raw = endpoint.unwrap_or(DEFAULT_ENDPOINT).trim();
        let url = Url::parse(raw).map_err(|_| {
            AiError::Invalid("the Anthropic endpoint is not a valid URL".to_owned())
        })?;
        if !matches!(url.scheme(), "http" | "https") || url.host().is_none() {
            return Err(AiError::Invalid(
                "the Anthropic endpoint must be an http or https URL with a host".to_owned(),
            ));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(AiError::Invalid(
                "the Anthropic endpoint must not contain a user name or password".to_owned(),
            ));
        }
        if url.query().is_some() || url.fragment().is_some() {
            return Err(AiError::Invalid(
                "the Anthropic endpoint must not contain a query or fragment".to_owned(),
            ));
        }
        let loopback = host_is_loopback(&url);
        if url.scheme() == "http" && !loopback {
            return Err(AiError::Invalid(
                "the Anthropic endpoint must use https unless it is a loopback address".to_owned(),
            ));
        }
        let client = build_client(loopback, READ_TIMEOUT)?;
        Ok(Self {
            client,
            endpoint: raw.trim_end_matches('/').to_owned(),
            key,
            loopback,
        })
    }

    /// Sets how long a response may stay silent (no byte received) before the
    /// request fails with `Network("timed out")`. The default is 120 seconds.
    ///
    /// # Errors
    ///
    /// [`AiError::Provider`] when the HTTP client cannot be rebuilt.
    pub fn with_read_timeout(mut self, timeout: Duration) -> Result<Self, AiError> {
        self.client = build_client(self.loopback, timeout)?;
        Ok(self)
    }

    /// The endpoint as configured, without a trailing `/`.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Always false: Anthropic is a remote provider.
    #[must_use]
    // An inherent method like Ollama's, so callers need not know the type.
    #[allow(clippy::unused_self)]
    pub fn is_local(&self) -> bool {
        false
    }

    /// What one model supports, from a static table (no I/O).
    ///
    /// Ids starting with `claude-` have tools and vision and a 200,000 token
    /// context (the smallest window of any listed model; `list_models` reports
    /// the exact `max_input_tokens`). Other ids have tools only and an unknown
    /// window. JSON mode is always off.
    #[must_use]
    #[allow(clippy::unused_self)]
    pub fn model_capabilities(&self, model: &str) -> Capabilities {
        model_capabilities_for(model)
    }

    fn key_text(&self) -> &str {
        self.key.to_str().unwrap_or_default()
    }

    fn request(&self, method: Method, path: &str) -> RequestBuilder {
        self.client
            .request(method, format!("{}{path}", self.endpoint))
            .header(KEY_HEADER, self.key.clone())
            .header(VERSION_HEADER, HeaderValue::from_static(API_VERSION))
            .header(CONTENT_TYPE, "application/json")
    }

    async fn read_json<T: DeserializeOwned>(&self, req: RequestBuilder) -> Result<T, AiError> {
        let mut resp = req
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(from_reqwest)?;
        if !resp.status().is_success() {
            return Err(from_response(resp, self.key_text()).await);
        }
        let bytes = read_capped(&mut resp, MAX_JSON_BODY).await?;
        serde_json::from_slice(&bytes).map_err(|e| malformed("JSON response", &e))
    }
}

fn build_client(loopback: bool, read_timeout: Duration) -> Result<Client, AiError> {
    let mut builder = Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(read_timeout)
        .redirect(Policy::none());
    if loopback {
        // A loopback server must not be reached through an environment proxy.
        builder = builder.no_proxy();
    }
    builder
        .build()
        .map_err(|_| AiError::Provider("cannot build the HTTP client".to_owned()))
}

fn host_is_loopback(url: &Url) -> bool {
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

impl AiProvider for AnthropicProvider {
    fn id(&self) -> &'static str {
        "anthropic"
    }

    fn list_models(&self) -> BoxFuture<'_, Result<Vec<ModelInfo>, AiError>> {
        Box::pin(async move {
            let mut models = Vec::new();
            let mut after: Option<String> = None;
            for _ in 0..MAX_MODEL_PAGES {
                let mut req = self
                    .request(Method::GET, "/v1/models")
                    .query(&[("limit", MODELS_PAGE_SIZE)]);
                if let Some(id) = &after {
                    req = req.query(&[("after_id", id.as_str())]);
                }
                let page: ModelsPage = self.read_json(req).await?;
                for m in page.data {
                    let window = m
                        .max_input_tokens
                        .and_then(|n| u32::try_from(n).ok())
                        .or_else(|| model_capabilities_for(&m.id).max_context_tokens);
                    let mut info = ModelInfo::new(m.id);
                    if let Some(name) = m.display_name.filter(|n| !n.is_empty()) {
                        info = info.with_display_name(name);
                    }
                    if let Some(n) = window {
                        info = info.with_context_window(n);
                    }
                    models.push(info);
                }
                match page.last_id {
                    Some(id) if page.has_more => after = Some(id),
                    _ => break,
                }
            }
            Ok(models)
        })
    }

    fn stream(&self, request: ChatRequest) -> Box<dyn ChatStream + '_> {
        match request_body(&request) {
            Err(e) => Box::new(AnthropicStream::failed(e, self.key.clone())),
            Ok(body) => {
                let fut = self
                    .request(Method::POST, "/v1/messages")
                    .header(ACCEPT, "text/event-stream")
                    .body(body)
                    .send();
                Box::new(AnthropicStream::sending(Box::pin(fut), self.key.clone()))
            }
        }
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::new(true, false, true, Some(200_000))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "sk-ant-test-0123456789";

    fn new(endpoint: &str) -> Result<AnthropicProvider, AiError> {
        AnthropicProvider::new(KEY, Some(endpoint))
    }

    #[test]
    fn defaults() {
        let p = AnthropicProvider::new(KEY, None).unwrap();
        assert_eq!(p.endpoint(), DEFAULT_ENDPOINT);
        assert_eq!(p.id(), "anthropic");
        assert!(!p.is_local());
        assert_eq!(
            p.capabilities(),
            Capabilities::new(true, false, true, Some(200_000))
        );
        assert_eq!(new("https://h:1/").unwrap().endpoint(), "https://h:1");
    }

    #[test]
    fn rejects_bad_keys_without_echoing_them() {
        for bad in ["", "   ", "a\nb", "k\u{e9}y", "a\u{0}b"] {
            let e = AnthropicProvider::new(bad, None).unwrap_err();
            assert!(matches!(&e, AiError::Invalid(_)), "{bad:?}");
            if !bad.trim().is_empty() {
                assert_eq!(
                    e,
                    AiError::Invalid("the Anthropic API key contains invalid characters".into())
                );
            }
        }
        assert_eq!(
            AnthropicProvider::new(" ", None).unwrap_err(),
            AiError::Invalid("the Anthropic API key is empty".into())
        );
    }

    #[test]
    fn endpoint_rules() {
        for bad in [
            "http://example.com",
            "http://10.0.0.1:1",
            "ftp://x",
            "not a url",
            "",
            "https://u:p@example.com",
            "https://u@example.com",
            "https://example.com/?token=abc",
            "https://example.com/#frag",
            "file:///tmp/x",
        ] {
            assert!(matches!(new(bad), Err(AiError::Invalid(_))), "{bad}");
        }
        for good in [
            "http://127.0.0.1:1",
            "http://localhost:1",
            "http://[::1]:1",
            "https://example.com",
            "https://proxy.internal:8443/base",
        ] {
            assert!(new(good).is_ok(), "{good}");
        }
    }

    #[test]
    fn debug_redacts_the_key() {
        let p = new("http://127.0.0.1:1").unwrap();
        let d = format!("{p:?}");
        assert!(!d.contains(KEY));
        assert!(d.contains("<redacted>"));
        assert!(d.contains("http://127.0.0.1:1"));
    }

    #[test]
    fn model_table_is_static() {
        let p = AnthropicProvider::new(KEY, None).unwrap();
        assert_eq!(
            p.model_capabilities("claude-opus-5-5"),
            Capabilities::new(true, false, true, Some(200_000))
        );
        assert_eq!(
            p.model_capabilities("other"),
            Capabilities::new(true, false, false, None)
        );
    }

    #[test]
    fn is_send_sync() {
        fn check<T: Send + Sync>() {}
        check::<AnthropicProvider>();
    }
}
