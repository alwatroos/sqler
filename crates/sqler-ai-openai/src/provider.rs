// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `OpenAiProvider`: the `AiProvider` implementation.

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
use crate::stream::OpenAiStream;
use crate::wire::Models;

/// The base URL of the official `OpenAI` API.
pub const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const READ_TIMEOUT: Duration = Duration::from_secs(120);
const MODELS_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_JSON_BODY: usize = 16 * 1024 * 1024;
const OPENAI_HOST: &str = "api.openai.com";

/// Which dialect of the Chat Completions API a server speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Flavor {
    /// The official API, recognised by the host `api.openai.com`. Output is
    /// capped with `max_completion_tokens`.
    OpenAi,
    /// Any other server (LM Studio, vLLM, the llama.cpp server, a gateway).
    /// Output is capped with `max_tokens`, and vision is assumed off.
    Compatible,
}

/// An `AiProvider` for `OpenAI` and any server that speaks its Chat
/// Completions API.
///
/// # Tokio
///
/// The futures and streams of this type must be polled inside a Tokio runtime
/// with I/O and time enabled.
///
/// # Base URL and flavour
///
/// The base URL includes the version path, for example
/// `https://api.openai.com/v1` or `http://localhost:1234/v1`; the provider
/// appends `/models` and `/chat/completions`. The host `api.openai.com`
/// selects [`Flavor::OpenAi`]; every other host is [`Flavor::Compatible`].
///
/// # Locality
///
/// [`is_local`](Self::is_local) is true when the host is `localhost` or a
/// loopback address. A local host is never reached through an environment
/// proxy.
///
/// # API key
///
/// The key is optional: a local server may need none. It is sent as
/// `Authorization: Bearer <key>` on every request, held as a sensitive header
/// value, and never appears in `Debug`, in errors or in logs (this crate does
/// not log). An API key over `http://` to a host that is not local is refused.
/// Redirects are not followed, so the key never goes to a second host. The
/// key is not zeroized in memory; it also lives in the keychain layer.
///
/// # Capabilities
///
/// [`AiProvider::capabilities`] returns what
/// [`with_capabilities`](Self::with_capabilities) set. Otherwise the
/// official API reports tools, JSON mode and vision; a compatible server
/// reports tools and JSON mode, without vision.
#[derive(Clone)]
pub struct OpenAiProvider {
    client: Client,
    base: String,
    auth: Option<HeaderValue>,
    flavor: Flavor,
    local: bool,
    caps: Option<Capabilities>,
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

impl OpenAiProvider {
    /// Creates a provider. `None` selects [`DEFAULT_BASE_URL`]. An empty key
    /// counts as no key.
    ///
    /// # Errors
    ///
    /// [`AiError::Invalid`] when the base URL is not an `http` or `https` URL
    /// with a host and without user info, query or fragment, when the key has
    /// characters not allowed in a header, or when a key would travel over
    /// `http://` to a host that is not local. The messages never quote the URL
    /// or the key.
    pub fn new(base_url: Option<&str>, api_key: Option<&str>) -> Result<Self, AiError> {
        let raw = base_url.unwrap_or(DEFAULT_BASE_URL).trim();
        if raw.len() > 2048 || raw.chars().any(char::is_whitespace) {
            return Err(AiError::Invalid(
                "the base URL is not a valid URL".to_owned(),
            ));
        }
        let url = Url::parse(raw)
            .map_err(|_| AiError::Invalid("the base URL is not a valid URL".to_owned()))?;
        if !matches!(url.scheme(), "http" | "https") || url.host().is_none() {
            return Err(AiError::Invalid(
                "the base URL must be an http or https URL with a host".to_owned(),
            ));
        }
        if !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || raw.contains(['@', '?', '#'])
        {
            return Err(AiError::Invalid(
                "the base URL must not contain credentials, a query or a fragment".to_owned(),
            ));
        }
        let local = host_is_local(&url);
        let flavor = if url
            .host_str()
            .is_some_and(|h| h.eq_ignore_ascii_case(OPENAI_HOST))
        {
            Flavor::OpenAi
        } else {
            Flavor::Compatible
        };
        let auth = match api_key.map(str::trim).filter(|k| !k.is_empty()) {
            None => None,
            Some(key) => {
                if !key.bytes().all(|b| b.is_ascii_graphic()) {
                    return Err(AiError::Invalid(
                        "the API key contains characters not allowed in a header".to_owned(),
                    ));
                }
                if url.scheme() == "http" && !local {
                    return Err(AiError::Invalid(
                        "an API key needs https:// unless the host is local".to_owned(),
                    ));
                }
                let mut value = HeaderValue::from_str(&format!("Bearer {key}")).map_err(|_| {
                    AiError::Invalid(
                        "the API key contains characters not allowed in a header".to_owned(),
                    )
                })?;
                value.set_sensitive(true);
                Some(value)
            }
        };
        let client = build_client(local, READ_TIMEOUT)?;
        Ok(Self {
            client,
            base: raw.trim_end_matches('/').to_owned(),
            auth,
            flavor,
            local,
            caps: None,
        })
    }

    /// Overrides the capabilities this provider reports, for a compatible
    /// server whose abilities differ from the defaults.
    #[must_use]
    pub fn with_capabilities(mut self, caps: Capabilities) -> Self {
        self.caps = Some(caps);
        self
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

    /// The base URL as configured, without a trailing `/`.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base
    }

    /// Which dialect this provider speaks.
    #[must_use]
    pub fn flavor(&self) -> Flavor {
        self.flavor
    }

    /// Whether the base URL host is `localhost` or a loopback address, so no
    /// data leaves the machine.
    #[must_use]
    pub fn is_local(&self) -> bool {
        self.local
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// The key text, only to scrub it out of server error messages.
    fn key(&self) -> Option<String> {
        self.auth
            .as_ref()
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(str::to_owned)
    }

    fn authorize(&self, req: RequestBuilder) -> RequestBuilder {
        match &self.auth {
            Some(v) => req.header(AUTHORIZATION, v.clone()),
            None => req,
        }
    }

    async fn fetch_models(&self) -> Result<Models, AiError> {
        let req = self.authorize(self.client.get(self.url("/models")));
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

fn host_is_local(url: &Url) -> bool {
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

impl fmt::Debug for OpenAiProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenAiProvider")
            .field("base_url", &self.base)
            .field("flavor", &self.flavor)
            .field("local", &self.local)
            .field("api_key", &self.auth.as_ref().map(|_| "***"))
            .finish_non_exhaustive()
    }
}

impl AiProvider for OpenAiProvider {
    fn id(&self) -> &'static str {
        "openai"
    }

    fn list_models(&self) -> BoxFuture<'_, Result<Vec<ModelInfo>, AiError>> {
        Box::pin(async move {
            let list = self.fetch_models().await?;
            let mut models: Vec<ModelInfo> = list
                .data
                .into_iter()
                .map(|m| {
                    let window = m
                        .context_length
                        .or(m.max_model_len)
                        .or_else(|| m.meta.and_then(|meta| meta.n_ctx_train))
                        .and_then(|n| u32::try_from(n).ok());
                    let info = ModelInfo::new(m.id);
                    match window {
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
        match request_body(&request, self.flavor) {
            Err(e) => Box::new(OpenAiStream::failed(e)),
            Ok(body) => {
                let fut = self
                    .authorize(self.client.post(self.url("/chat/completions")))
                    .header(CONTENT_TYPE, "application/json")
                    .body(body)
                    .send();
                Box::new(OpenAiStream::sending(Box::pin(fut), self.key()))
            }
        }
    }

    fn capabilities(&self) -> Capabilities {
        self.caps.unwrap_or_else(|| match self.flavor {
            Flavor::OpenAi => Capabilities::new(true, true, true, None),
            Flavor::Compatible => Capabilities::new(true, true, false, None),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make(url: &str) -> OpenAiProvider {
        OpenAiProvider::new(Some(url), None).unwrap()
    }

    #[test]
    fn flavor_detection() {
        let p = OpenAiProvider::new(None, None).unwrap();
        assert_eq!(p.flavor(), Flavor::OpenAi);
        assert_eq!(p.base_url(), DEFAULT_BASE_URL);
        assert_eq!(p.id(), "openai");
        assert!(!p.is_local());
        assert_eq!(make("https://API.OpenAI.com/v1/").flavor(), Flavor::OpenAi);
        assert_eq!(
            make("https://api.openai.com.evil.test/v1").flavor(),
            Flavor::Compatible
        );
        assert_eq!(
            make("https://proxy.example.com/v1").flavor(),
            Flavor::Compatible
        );
        assert_eq!(
            make("http://localhost:1234/v1").flavor(),
            Flavor::Compatible
        );
    }

    #[test]
    fn base_url_is_trimmed() {
        assert_eq!(make(" http://h:1/v1/ ").base_url(), "http://h:1/v1");
    }

    #[test]
    fn locality() {
        for url in [
            "http://localhost:1234/v1",
            "http://LOCALHOST.:1234/v1",
            "http://127.0.0.2:1/v1",
            "http://[::1]:1234/v1",
        ] {
            assert!(make(url).is_local(), "{url}");
        }
        for url in ["http://10.0.0.1:1234/v1", "https://example.com/v1"] {
            assert!(!make(url).is_local(), "{url}");
        }
    }

    #[test]
    fn rejects_bad_base_urls() {
        for bad in [
            "ftp://x/v1",
            "not a url",
            "",
            "http://",
            "file:///tmp/x",
            "http://user:pw@localhost/v1",
            "http://user@localhost/v1",
            "http://localhost/v1?q=1",
            "http://localhost/v1#f",
            "http://local\thost/v1",
            "http://local host/v1",
        ] {
            let e = OpenAiProvider::new(Some(bad), None).unwrap_err();
            assert!(matches!(e, AiError::Invalid(_)), "{bad}");
            assert!(!format!("{e}").contains("pw"));
        }
    }

    #[test]
    fn rejects_embedded_control_and_overlong_urls() {
        for bad in ["http://loc\talhost/v1", "http://localhost/v\n1"] {
            assert!(OpenAiProvider::new(Some(bad), None).is_err(), "{bad:?}");
        }
        let long = format!("http://localhost/{}", "a".repeat(2048));
        assert!(OpenAiProvider::new(Some(&long), None).is_err());
    }

    #[test]
    fn key_rules() {
        let key = Some("sk-secret");
        let e = OpenAiProvider::new(Some("http://example.com/v1"), key).unwrap_err();
        assert_eq!(
            e,
            AiError::Invalid("an API key needs https:// unless the host is local".into())
        );
        assert!(OpenAiProvider::new(Some("http://localhost:1234/v1"), key).is_ok());
        assert!(OpenAiProvider::new(Some("http://example.com/v1"), None).is_ok());
        assert!(OpenAiProvider::new(Some("http://example.com/v1"), Some("  ")).is_ok());
        assert!(OpenAiProvider::new(Some("https://example.com/v1"), key).is_ok());
        for bad in ["sk bad", "sk\nbad", "sk-é"] {
            let e = OpenAiProvider::new(None, Some(bad)).unwrap_err();
            assert_eq!(
                e,
                AiError::Invalid("the API key contains characters not allowed in a header".into())
            );
        }
    }

    #[test]
    fn debug_hides_the_key() {
        let p = OpenAiProvider::new(None, Some("sk-secret")).unwrap();
        let shown = format!("{p:?}");
        assert!(!shown.contains("sk-secret"));
        assert!(shown.contains("***"));
        let none = format!("{:?}", OpenAiProvider::new(None, None).unwrap());
        assert!(none.contains("None"));
    }

    #[test]
    fn default_and_overridden_capabilities() {
        let o = OpenAiProvider::new(None, None).unwrap();
        assert_eq!(o.capabilities(), Capabilities::new(true, true, true, None));
        let c = make("http://localhost:1/v1");
        assert_eq!(c.capabilities(), Capabilities::new(true, true, false, None));
        let custom = Capabilities::new(false, true, true, Some(8192));
        assert_eq!(c.with_capabilities(custom).capabilities(), custom);
    }

    #[test]
    fn is_send_sync() {
        fn check<T: Send + Sync>() {}
        check::<OpenAiProvider>();
    }
}
