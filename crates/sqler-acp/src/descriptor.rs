// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `MCP` server descriptor handed to an agent at session creation.
//!
//! Two shapes match the transports of `SQLer`'s `MCP` server: a stdio command the agent starts itself, or a
//! loopback HTTP endpoint with a bearer token. The token travels only in the `Authorization` header of the
//! `session/new` message. It is never part of an argument, the agent's environment, a log line or an error.

use std::fmt;
use std::path::PathBuf;

use agent_client_protocol::schema::v1::{
    EnvVariable, HttpHeader, McpServer, McpServerHttp, McpServerStdio,
};
use zeroize::Zeroizing;

use crate::error::AcpError;

/// Header that carries the token (value: `Bearer <token>`).
pub const MCP_AUTH_HEADER: &str = "Authorization";

/// A secret for the HTTP `MCP` endpoint. Zeroized on drop; `Debug` never prints it.
#[derive(Clone)]
pub struct McpToken(Zeroizing<String>);

impl McpToken {
    /// Wrap a token.
    #[must_use]
    pub fn new(token: String) -> Self {
        Self(Zeroizing::new(token))
    }

    /// The token text. Call this only to build the header.
    #[must_use]
    pub fn expose(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Debug for McpToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("McpToken(***)")
    }
}

/// An `MCP` server the agent may use in a session.
#[derive(Debug, Clone)]
pub enum McpServerDescriptor {
    /// The agent starts `command` and talks `MCP` over its stdio.
    ///
    /// `args` and `env` go to the agent verbatim. Do not put credentials there: use [`Self::Http`] with a token.
    Stdio {
        /// Server name shown to the agent.
        name: String,
        /// Program to run.
        command: PathBuf,
        /// Arguments.
        args: Vec<String>,
        /// Environment variables for that program.
        env: Vec<(String, String)>,
    },
    /// A loopback `MCP` endpoint with a bearer token. The host of `url` must be `127.0.0.1`, `[::1]` or
    /// `localhost`.
    Http {
        /// Server name shown to the agent.
        name: String,
        /// Endpoint URL.
        url: String,
        /// Bearer token.
        token: McpToken,
    },
}

/// Whether the host of `url` is a loopback name (`127.0.0.1`, `[::1]`, `localhost`).
pub(crate) fn is_loopback_url(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once("://") else {
        return false;
    };
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return false;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.contains('@') {
        return false;
    }
    let (host, port) = if let Some(inner) = authority.strip_prefix('[') {
        match inner.split_once(']') {
            Some((host, "")) => (host, None),
            Some((host, tail)) => match tail.strip_prefix(':') {
                Some(port) => (host, Some(port)),
                None => return false,
            },
            None => return false,
        }
    } else {
        match authority.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        }
    };
    if port.is_some_and(|p| p.is_empty() || p.parse::<u16>().is_err()) {
        return false;
    }
    host == "127.0.0.1" || host == "::1" || host.eq_ignore_ascii_case("localhost")
}

impl McpServerDescriptor {
    /// Check the descriptor: a name, and for HTTP a loopback URL.
    pub(crate) fn validate(&self) -> Result<(), AcpError> {
        match self {
            Self::Stdio { name, command, .. } => {
                if name.is_empty() || command.as_os_str().is_empty() {
                    return Err(AcpError::InvalidDescriptor(
                        "a stdio server needs a name and a command".to_owned(),
                    ));
                }
            }
            Self::Http { name, url, .. } => {
                if name.is_empty() {
                    return Err(AcpError::InvalidDescriptor(
                        "an HTTP server needs a name".to_owned(),
                    ));
                }
                if !is_loopback_url(url) {
                    return Err(AcpError::InvalidDescriptor(
                        "the HTTP endpoint must be on 127.0.0.1, [::1] or localhost".to_owned(),
                    ));
                }
            }
        }
        Ok(())
    }

    /// Whether this descriptor needs the agent's HTTP `MCP` capability.
    pub(crate) fn needs_http(&self) -> bool {
        matches!(self, Self::Http { .. })
    }

    /// Map to the protocol type. The token goes only into the header.
    pub(crate) fn to_acp(&self) -> McpServer {
        match self {
            Self::Stdio {
                name,
                command,
                args,
                env,
            } => McpServer::Stdio(
                McpServerStdio::new(name.clone(), command.clone())
                    .args(args.clone())
                    .env(
                        env.iter()
                            .map(|(k, v)| EnvVariable::new(k.clone(), v.clone()))
                            .collect(),
                    ),
            ),
            Self::Http { name, url, token } => {
                McpServer::Http(McpServerHttp::new(name.clone(), url.clone()).headers(vec![
                    HttpHeader::new(MCP_AUTH_HEADER, format!("Bearer {}", token.expose())),
                ]))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_hosts_are_accepted() {
        for url in [
            "http://127.0.0.1:8080/mcp",
            "http://localhost/mcp",
            "http://LOCALHOST:1/",
            "http://[::1]:9000/mcp",
            "https://127.0.0.1",
            "http://127.0.0.1?x=1",
        ] {
            assert!(is_loopback_url(url), "{url}");
        }
    }

    #[test]
    fn other_hosts_are_refused() {
        for url in [
            "http://example.com/mcp",
            "http://127.0.0.1.evil.com/",
            "http://localhost.evil.com/",
            "http://user@127.0.0.1/",
            "http://evil.com@127.0.0.1/",
            "http://10.0.0.1/",
            "http://0.0.0.0:80/",
            "http://[::2]/",
            "http://127.0.0.1:port/",
            "ftp://127.0.0.1/",
            "127.0.0.1:80",
            "http://[::1/",
            "http://[::1]evil/",
            "",
        ] {
            assert!(!is_loopback_url(url), "{url}");
        }
    }

    #[test]
    fn non_loopback_descriptor_is_invalid() {
        let d = McpServerDescriptor::Http {
            name: "sqler".to_owned(),
            url: "http://example.com/mcp".to_owned(),
            token: McpToken::new("tok".to_owned()),
        };
        let err = d.validate().expect_err("must refuse");
        assert!(!err.to_string().contains("tok"));
    }

    #[test]
    fn debug_redacts_the_token() {
        let d = McpServerDescriptor::Http {
            name: "sqler".to_owned(),
            url: "http://127.0.0.1:1/mcp".to_owned(),
            token: McpToken::new("super-secret".to_owned()),
        };
        let text = format!("{d:?}");
        assert!(text.contains("McpToken(***)"));
        assert!(!text.contains("super-secret"));
    }

    #[test]
    fn http_maps_to_a_bearer_header_only() {
        let d = McpServerDescriptor::Http {
            name: "sqler".to_owned(),
            url: "http://127.0.0.1:1/mcp".to_owned(),
            token: McpToken::new("abc".to_owned()),
        };
        let McpServer::Http(http) = d.to_acp() else {
            panic!("expected http");
        };
        assert_eq!(http.url, "http://127.0.0.1:1/mcp");
        assert_eq!(http.headers.len(), 1);
        assert_eq!(http.headers[0].name, "Authorization");
        assert_eq!(http.headers[0].value, "Bearer abc");
    }

    #[test]
    fn stdio_maps_command_args_env() {
        let d = McpServerDescriptor::Stdio {
            name: "sqler".to_owned(),
            command: PathBuf::from("/bin/sqler"),
            args: vec!["mcp".to_owned()],
            env: vec![("K".to_owned(), "V".to_owned())],
        };
        let McpServer::Stdio(s) = d.to_acp() else {
            panic!("expected stdio");
        };
        assert_eq!(s.command, PathBuf::from("/bin/sqler"));
        assert_eq!(s.args, ["mcp"]);
        assert_eq!(
            (s.env[0].name.as_str(), s.env[0].value.as_str()),
            ("K", "V")
        );
    }
}
