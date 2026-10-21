// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `MistralProvider`: the `AiProvider` implementation.

use std::fmt;
use std::net::IpAddr;
use std::time::Duration;

use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderValue};
use reqwest::redirect::Policy;
use reqwest::{Client, RequestBuilder, Url};
use sqler_ai_api::{
    AiError, AiProvider, BoxFuture, Capabilities, ChatRequest, ChatStream, ModelInfo,
};

use crate::convert::request_body;
use crate::error::{from_reqwest, from_response, malformed, read_capped};
use crate::stream::MistralStream;
use crate::wire::{Models, WireModel};

/// The endpoint of the Mistral API. The provider appends `/v1/...`.
pub const DEFAULT_ENDPOINT: &str = "https://api.mistral.ai";

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const READ_TIMEOUT: Duration = Duration::from_secs(120);
const MODELS_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_JSON_BODY: usize = 16 * 1024 * 1024;
const MAX_ENDPOINT_BYTES: usize = 2048;

/// An `AiProvider` for the Mistral API.
///
/// # Tokio
///
/// The futures and streams of this type must be polled inside a Tokio runtime
/// with I/O and time enabled.
///
/// # Remote
///
/// Mistral is a remote provider: prompts and results leave the machine.
/// [`is_local`](Self::is_local) is always false, even for a loopback endpoint
/// (a test server or a proxy).
///
/// # The API key
///
/// The key is sent as `Authorization: Bearer <key>` on every request, held as
/// a sensitive header value, and never appears in `Debug`, in errors or in
/// logs (this crate does not log). Server error text has the key replaced by
/// `***` before it is cut to 200 characters. Redirects are not followed, so
/// the key never goes to a second host. The key is not zeroized in memory; it
/// also lives in the keychain layer.
///
/// # Capabilities
///
/// [`AiProvider::capabilities`] reports tools, JSON mode and vision.
/// [`model_capabilities`](Self::model_capabilities) asks the server about one
/// model.
#[derive(Clone)]
pub struct MistralProvider {
    client: Client,
    endpoint: String,
    auth: HeaderValue,
    local: bool,
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

impl MistralProvider {
    /// Creates a provider. `None` selects [`DEFAULT_ENDPOINT`].
    ///
    /// # Errors
    ///
    /// [`AiError::Invalid`] when the key is empty or has a byte that is not a
    /// visible ASCII character (the key is never echoed), or when the endpoint
    /// is over 2048 bytes, holds whitespace, a control character, `@`, `?` or
    /// `#`, is not an `http` or `https` URL with a host, or is `http` to a
    /// host that is not loopback (so the key never travels in clear text). The
    /// messages never quote the URL.
    pub fn new(api_key: &str, endpoint: Option<&str>) -> Result<Self, AiError> {
        let key = api_key.trim();
        if key.is_empty() {
            return Err(AiError::Invalid("the Mistral API key is empty".to_owned()));
        }
        if !key.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(AiError::Invalid(
                "the Mistral API key contains invalid characters".to_owned(),
            ));
        }
        let mut auth = HeaderValue::from_str(&format!("Bearer {key}")).map_err(|_| {
            AiError::Invalid("the Mistral API key contains invalid characters".to_owned())
        })?;
        auth.set_sensitive(true);

        let raw = endpoint.unwrap_or(DEFAULT_ENDPOINT).trim();
        if raw.len() > MAX_ENDPOINT_BYTES
            || raw.chars().any(|c| c.is_whitespace() || c.is_control())
        {
            return Err(AiError::Invalid(
                "the Mistral endpoint is not a valid URL".to_owned(),
            ));
        }
        if raw.contains(['@', '?', '#']) {
            return Err(AiError::Invalid(
                "the Mistral endpoint must not contain credentials, a query or a fragment"
                    .to_owned(),
            ));
        }
        let url = Url::parse(raw)
            .map_err(|_| AiError::Invalid("the Mistral endpoint is not a valid URL".to_owned()))?;
        if !matches!(url.scheme(), "http" | "https") || url.host().is_none() {
            return Err(AiError::Invalid(
                "the Mistral endpoint must be an http or https URL with a host".to_owned(),
            ));
        }
        if !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(AiError::Invalid(
                "the Mistral endpoint must not contain credentials, a query or a fragment"
                    .to_owned(),
            ));
        }
        let loopback = host_is_loopback(&url);
        if url.scheme() == "http" && !loopback {
            return Err(AiError::Invalid(
                "the Mistral endpoint must use https unless it is a loopback address".to_owned(),
            ));
        }
        let client = build_client(loopback, READ_TIMEOUT)?;
        Ok(Self {
            client,
            endpoint: raw.trim_end_matches('/').to_owned(),
            auth,
            local: loopback,
        })
    }

    /// Sets how long a response may stay silent (no byte received) before the
    /// request fails with `Network("timed out")`. The default is 120 seconds.
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

    /// Always false: Mistral is a remote provider, even when the endpoint is a
    /// loopback address.
    #[must_use]
    pub fn is_local(&self) -> bool {
        false
    }

    /// What one model can do, from `GET /v1/models/{id}`: tools when it
    /// reports `function_calling`, vision when it reports `vision`, JSON mode
    /// always, and its `max_context_length`. A model card without a
    /// `capabilities` object gives tools on and vision off.
    ///
    /// The id is added as one path segment, so `/`, `?` and `..` in it cannot
    /// change the path.
    pub fn model_capabilities(&self, model: &str) -> BoxFuture<'_, Result<Capabilities, AiError>> {
        let model = model.to_owned();
        Box::pin(async move {
            if model.is_empty() || model == "." || model == ".." {
                return Err(AiError::Invalid("the model id is not valid".to_owned()));
            }
            let mut url = Url::parse(&self.endpoint)
                .map_err(|_| AiError::Invalid("the Mistral endpoint is not a valid URL".into()))?;
            url.path_segments_mut()
                .map_err(|()| AiError::Invalid("the Mistral endpoint is not a valid URL".into()))?
                .pop_if_empty()
                .extend(["v1", "models"])
                .push(&model);
            let card: WireModel = self.get_json(url.as_str()).await?;
            let (tools, vision) = card
                .capabilities
                .as_ref()
                .map_or((true, false), |c| (c.function_calling, c.vision));
            let window = card.max_context_length.and_then(|n| u32::try_from(n).ok());
            Ok(Capabilities::new(tools, true, vision, window))
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.endpoint)
    }

    /// The key text, only to scrub it out of server error messages.
    fn key(&self) -> Option<String> {
        self.auth
            .to_str()
            .ok()
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(str::to_owned)
    }

    fn authorize(&self, req: RequestBuilder) -> RequestBuilder {
        req.header(AUTHORIZATION, self.auth.clone())
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T, AiError> {
        let req = self.authorize(self.client.get(url));
        let mut resp = req
            .timeout(MODELS_TIMEOUT)
            .send()
            .await
            .map_err(from_reqwest)?;
        if !resp.status().is_success() {
            return Err(from_response(resp, self.key().as_deref()).await);
        }
        let bytes = read_capped(&mut resp, MAX_JSON_BODY).await?;
        serde_json::from_slice(&bytes).map_err(|e| malformed("JSON response", &e))
    }
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
    bare.trim_end_matches('.').eq_ignore_ascii_case("localhost")
}

impl fmt::Debug for MistralProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MistralProvider")
            .field("endpoint", &self.endpoint)
            .field("api_key", &"***")
            .finish_non_exhaustive()
    }
}

impl AiProvider for MistralProvider {
    fn id(&self) -> &'static str {
        "mistral"
    }

    fn list_models(&self) -> BoxFuture<'_, Result<Vec<ModelInfo>, AiError>> {
        Box::pin(async move {
            let list: Models = self.get_json(&self.url("/v1/models")).await?;
            let mut models: Vec<ModelInfo> = list
                .data
                .into_iter()
                .filter(|m| {
                    m.capabilities
                        .as_ref()
                        .is_none_or(|c| c.completion_chat != Some(false))
                })
                .map(|m| {
                    let name = m.name.filter(|n| !n.is_empty() && *n != m.id);
                    let mut info = ModelInfo::new(m.id);
                    if let Some(name) = name {
                        info = info.with_display_name(name);
                    }
                    match m.max_context_length.and_then(|n| u32::try_from(n).ok()) {
                        Some(n) => info.with_context_window(n),
                        None => info,
                    }
                })
                .collect();
            models.sort_by(|a, b| a.id.cmp(&b.id));
            Ok(models)
        })
    }

    fn stream(&self, request: ChatRequest) -> Box<dyn ChatStream + '_> {
        match request_body(&request) {
            Err(e) => Box::new(MistralStream::failed(e)),
            Ok(body) => {
                let fut = self
                    .authorize(self.client.post(self.url("/v1/chat/completions")))
                    .header(CONTENT_TYPE, "application/json")
                    .body(body)
                    .send();
                Box::new(MistralStream::sending(Box::pin(fut), self.key()))
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

    const KEY: &str = "mst-secret";

    fn make(url: &str) -> MistralProvider {
        MistralProvider::new(KEY, Some(url)).unwrap()
    }

    #[test]
    fn defaults() {
        let p = MistralProvider::new(KEY, None).unwrap();
        assert_eq!(p.endpoint(), DEFAULT_ENDPOINT);
        assert_eq!(p.id(), "mistral");
        assert!(!p.is_local());
        assert_eq!(p.capabilities(), Capabilities::new(true, true, true, None));
    }

    #[test]
    fn is_local_is_false_even_for_loopback() {
        assert!(!make("http://127.0.0.1:1").is_local());
        assert!(!make("http://localhost:1").is_local());
    }

    #[test]
    fn endpoint_is_trimmed() {
        assert_eq!(
            make(" http://localhost:1/ ").endpoint(),
            "http://localhost:1"
        );
        assert_eq!(
            make("https://gw.example.com/prefix/").endpoint(),
            "https://gw.example.com/prefix"
        );
    }

    #[test]
    fn loopback_detection() {
        for url in [
            "http://localhost:1234",
            "http://LOCALHOST.:1234",
            "http://127.0.0.2:1",
            "http://[::1]:1234",
        ] {
            assert!(MistralProvider::new(KEY, Some(url)).is_ok(), "{url}");
        }
        for url in [
            "http://10.0.0.1:1234",
            "http://example.com",
            "http://localhost.evil.test",
        ] {
            assert!(MistralProvider::new(KEY, Some(url)).is_err(), "{url}");
        }
        assert!(MistralProvider::new(KEY, Some("https://example.com")).is_ok());
    }

    #[test]
    fn rejects_bad_endpoints() {
        let long = format!("http://localhost/{}", "a".repeat(2048));
        let table: Vec<&str> = vec![
            "ftp://x",
            "not a url",
            "",
            "http://",
            "file:///tmp/x",
            "http://user:pw@localhost",
            "http://user@localhost",
            "http://localhost/v1?q=1",
            "http://localhost/v1?",
            "http://localhost/v1#f",
            "http://localhost/v1#",
            "http://local\thost",
            "http://local host",
            "http://localhost/v\n1",
            "http://loc\u{0}alhost",
            "http://example.com",
            "https://exa mple.com",
            &long,
        ];
        for bad in table {
            let e = MistralProvider::new(KEY, Some(bad)).unwrap_err();
            assert!(matches!(e, AiError::Invalid(_)), "{bad:?}");
            let shown = format!("{e} {e:?}");
            assert!(!shown.contains("pw") && !shown.contains(KEY), "{bad:?}");
        }
    }

    #[test]
    fn rejects_bad_keys_without_echoing_them() {
        for bad in [
            "",
            "   ",
            "mst bad",
            "mst\nbad",
            "mst-é",
            "mst\u{7f}x",
            "a\tb",
        ] {
            let e = MistralProvider::new(bad, None).unwrap_err();
            assert!(matches!(e, AiError::Invalid(_)), "{bad:?}");
            let shown = format!("{e} {e:?}");
            assert!(
                !shown.contains("mst-é") && !shown.contains("bad"),
                "{bad:?}"
            );
        }
        // Surrounding whitespace is trimmed.
        assert!(MistralProvider::new("  mst-ok\n", None).is_ok());
    }

    #[test]
    fn debug_hides_the_key() {
        let p = MistralProvider::new(KEY, None).unwrap();
        let shown = format!("{p:?}");
        assert!(!shown.contains(KEY));
        assert!(shown.contains("api_key: \"***\""));
        assert!(shown.contains(DEFAULT_ENDPOINT));
    }

    #[test]
    fn key_for_scrubbing_is_the_raw_key() {
        assert_eq!(make("http://localhost:1").key().as_deref(), Some(KEY));
    }

    #[test]
    fn read_timeout_can_be_changed() {
        assert!(
            make("http://localhost:1")
                .with_read_timeout(Duration::from_millis(5))
                .is_ok()
        );
    }

    #[test]
    fn is_send_sync() {
        fn check<T: Send + Sync>() {}
        check::<MistralProvider>();
    }
}
