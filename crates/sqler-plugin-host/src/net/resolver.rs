// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The vetted resolver: one resolution per connection, every address checked, the vetted list is what gets
//! connected to (no second lookup, so DNS rebinding has no window).

use std::fmt;
use std::future::Future;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use reqwest::dns::{Addrs, Name, Resolve, Resolving};

use super::addr::is_forbidden;

/// Longest a name lookup may take.
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(10);

/// The future of one lookup.
pub type LookupFuture = Pin<Box<dyn Future<Output = io::Result<Vec<IpAddr>>> + Send>>;

/// Name to addresses. A seam for tests; production uses [`SystemResolver`].
pub trait Resolver: Send + Sync {
    /// All addresses of `host` (a normalised name, never an IP literal).
    fn lookup(&self, host: &str) -> LookupFuture;
}

/// The operating system's resolver (`getaddrinfo` through tokio).
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemResolver;

impl Resolver for SystemResolver {
    fn lookup(&self, host: &str) -> LookupFuture {
        let target = format!("{host}:0");
        Box::pin(async move {
            let addrs = tokio::net::lookup_host(target).await?;
            Ok(addrs.map(|a| a.ip()).collect())
        })
    }
}

/// Why a name was refused or failed; carried inside the reqwest error and recovered by the egress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum VetError {
    /// An address of the name is forbidden.
    Forbidden(IpAddr),
    /// The name has no address.
    NoAddress,
    /// The lookup failed or timed out.
    Lookup,
}

impl fmt::Display for VetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Forbidden(addr) => write!(f, "address {addr} is not reachable from a plugin"),
            Self::NoAddress => f.write_str("name resolves to no address"),
            Self::Lookup => f.write_str("name lookup failed"),
        }
    }
}

impl std::error::Error for VetError {}

/// Refuses the list if it is empty or if **any** address is forbidden; otherwise returns it unchanged.
///
/// # Errors
/// [`DenyReason`](crate::DenyReason)-shaped: `Err(Some(addr))` for the first forbidden address, `Err(None)` for an
/// empty list.
pub fn vet(addrs: Vec<IpAddr>) -> Result<Vec<IpAddr>, Option<IpAddr>> {
    if addrs.is_empty() {
        return Err(None);
    }
    match addrs.iter().find(|a| is_forbidden(**a)) {
        Some(bad) => Err(Some(*bad)),
        None => Ok(addrs),
    }
}

/// The resolver the reqwest client uses.
///
/// `localhost` is answered with `127.0.0.1` and `::1` without a lookup and without the address check: the grant
/// check (`check_host`) lets that name through only for an exact `localhost` pattern. Any other name is looked up
/// through the [`Resolver`] and refused when any answer is forbidden.
#[derive(Clone)]
pub struct VettedResolver {
    inner: Arc<dyn Resolver>,
}

impl fmt::Debug for VettedResolver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("VettedResolver")
    }
}

impl VettedResolver {
    /// A resolver over `inner`.
    #[must_use]
    pub fn new(inner: Arc<dyn Resolver>) -> Self {
        Self { inner }
    }

    /// Resolves and vets `host`.
    pub(super) async fn resolve_host(&self, host: &str) -> Result<Vec<IpAddr>, VetError> {
        if host == "localhost" {
            return Ok(vec![
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                IpAddr::V6(Ipv6Addr::LOCALHOST),
            ]);
        }
        let looked_up = tokio::time::timeout(LOOKUP_TIMEOUT, self.inner.lookup(host))
            .await
            .map_err(|_| VetError::Lookup)?
            .map_err(|_| VetError::Lookup)?;
        vet(looked_up).map_err(|bad| bad.map_or(VetError::NoAddress, VetError::Forbidden))
    }
}

impl Resolve for VettedResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let this = self.clone();
        Box::pin(async move {
            let host = name.as_str().to_ascii_lowercase();
            let addrs = this
                .resolve_host(&host)
                .await
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;
            let iter: Addrs = Box::new(
                addrs
                    .into_iter()
                    .map(|ip| SocketAddr::new(ip, 0))
                    .collect::<Vec<_>>()
                    .into_iter(),
            );
            Ok(iter)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed(Vec<IpAddr>);
    impl Resolver for Fixed {
        fn lookup(&self, _host: &str) -> LookupFuture {
            let v = self.0.clone();
            Box::pin(async move { Ok(v) })
        }
    }

    struct Panics;
    impl Resolver for Panics {
        fn lookup(&self, _host: &str) -> LookupFuture {
            panic!("localhost must not be looked up")
        }
    }

    fn ips(list: &[&str]) -> Vec<IpAddr> {
        list.iter().map(|s| s.parse().unwrap()).collect()
    }

    async fn run(list: &[&str]) -> Result<Vec<IpAddr>, VetError> {
        VettedResolver::new(Arc::new(Fixed(ips(list))))
            .resolve_host("rebind.example.com")
            .await
    }

    #[tokio::test]
    async fn public_answers_pass_unchanged() {
        assert_eq!(
            run(&["93.184.216.34", "2606:2800:220:1::1"]).await,
            Ok(ips(&["93.184.216.34", "2606:2800:220:1::1"]))
        );
    }

    #[tokio::test]
    async fn any_forbidden_address_refuses_the_name() {
        for list in [
            &["127.0.0.1"][..],
            &["10.1.2.3"],
            &["::1"],
            &["8.8.8.8", "127.0.0.1"],
            &["127.0.0.1", "8.8.8.8"],
            &["::ffff:192.168.0.1"],
        ] {
            assert!(
                matches!(run(list).await, Err(VetError::Forbidden(_))),
                "{list:?}"
            );
        }
        assert_eq!(
            run(&["8.8.8.8", "169.254.169.254"]).await,
            Err(VetError::Forbidden("169.254.169.254".parse().unwrap()))
        );
    }

    #[tokio::test]
    async fn empty_answer_is_refused() {
        assert_eq!(run(&[]).await, Err(VetError::NoAddress));
    }

    #[tokio::test]
    async fn lookup_error_is_a_lookup_failure() {
        struct Fails;
        impl Resolver for Fails {
            fn lookup(&self, _host: &str) -> LookupFuture {
                Box::pin(async { Err(io::Error::other("nxdomain")) })
            }
        }
        let r = VettedResolver::new(Arc::new(Fails));
        assert_eq!(r.resolve_host("x.example.com").await, Err(VetError::Lookup));
    }

    #[tokio::test]
    async fn localhost_is_answered_without_a_lookup() {
        let r = VettedResolver::new(Arc::new(Panics));
        assert_eq!(
            r.resolve_host("localhost").await,
            Ok(ips(&["127.0.0.1", "::1"]))
        );
        // Not exactly localhost: goes to the (panicking) lookup path, never answered locally.
        let r = VettedResolver::new(Arc::new(Fixed(ips(&["127.0.0.1"]))));
        assert!(r.resolve_host("localhost.example.com").await.is_err());
        assert!(r.resolve_host("127.0.0.1.nip.io").await.is_err());
    }
}
