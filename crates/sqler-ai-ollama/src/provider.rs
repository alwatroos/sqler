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

use crate::convert::{capabilities_from_show, context_length, request_body};
use crate::error::{from_reqwest, from_response, read_capped};
use crate::stream::OllamaStream;
use crate::wire::{Show, ShowBody, Tags};

/// The endpoint of a default Ollama install.
pub const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:11434";

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
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
    /// with a host, or when it carries a user name or password.
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
        let local = host_is_local(&url);
        let mut builder = Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .redirect(Policy::none());
        if local {
            // A loopback server must not be reached through an environment proxy.
            builder = builder.no_proxy();
        }
        let client = builder
            .build()
            .map_err(|_| AiError::Provider("cannot build the HTTP client".to_owned()))?;
        Ok(Self {
            client,
            endpoint: raw.trim_end_matches('/').to_owned(),
            local,
        })
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
        serde_json::from_slice(&bytes)
            .map_err(|e| AiError::Protocol(format!("malformed JSON response: {e}")))
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
            let mut models = Vec::with_capacity(tags.models.len());
            for m in tags.models {
                let mut info = ModelInfo::new(m.name.clone());
                // A failed `show` only leaves the window unknown.
                if let Some(n) = self
                    .show(&m.name)
                    .await
                    .ok()
                    .as_ref()
                    .and_then(context_length)
                {
                    info = info.with_context_window(n);
                }
                models.push(info);
            }
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
    fn is_send_sync() {
        fn check<T: Send + Sync>() {}
        check::<OllamaProvider>();
    }
}
