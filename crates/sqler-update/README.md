# sqler-update

Check-only update client: fetches one signed manifest from GitHub Releases and reports a newer version.

Status: implemented (M7, R3)

Layer: L1 (depends on no other `sqler-*` crate)

```rust,ignore
use sqler_update::{CheckOutcome, UpdateClient, DEFAULT_FEED_URL};

let client = UpdateClient::new(DEFAULT_FEED_URL, semver::Version::parse(env!("CARGO_PKG_VERSION"))?)?;
match client.check().await? {
    CheckOutcome::Available(offer) => println!("{} is available: {}", offer.version, offer.notes_url),
    CheckOutcome::UpToDate { .. } => {}
}
```

- Check-only: it never downloads or installs a binary. The offer links to the release notes and carries the
  artifact entry for this platform, which the caller may show but this crate does not fetch.
- Opt-in: the caller decides whether and when to check. `CheckSchedule` holds the timing (first check at least 30 s
  after the first frame, then every 24 h, backoff from 1 h up to 24 h after failures).
- The asset is an envelope `{format, key_id, signature, payload}`: an Ed25519 signature (verified with `ring`)
  over `SIGNING_CONTEXT` followed by the manifest bytes. The payload is parsed only after it verifies.
- Network access goes only to the configured feed URL (and its redirects: `https`, at most 3). The only request
  header of its own is `User-Agent: SQLer/<version> (update-check)`. No telemetry, no identifiers.
- Not configured yet: the list of trusted keys (`keys/`) is empty until the production key is committed (R17).
  Until then `check()` returns `UpdateError::NotConfigured` without any request.
- Tests use a loopback mock server and TEST-ONLY keys that exist only in `tests/support`; never a release key.

See [AGENTS.md](../../AGENTS.md) for the project handbook and ADR-0066 for the format and policy.
