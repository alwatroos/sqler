# sqler-update

Update client: fetches one signed manifest from GitHub Releases, reports a newer version, downloads the artifact
for this platform and installs it.

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

- Check: the offer links to the release notes and carries the artifact entries for this platform (`download`,
  and on macOS `bundle`, the signed `.dmg` from the manifest field `bundles`).
- Download (`UpdateClient::download`): only from the pinned release URL
  `github.com/alwatroos/sqler/releases/download/v<version>/<name>`, into `updates/<version>/`. The size and sha256 of
  the signed manifest are checked while streaming, before the file gets its final name. A failure removes the
  partial file.
- Install (`plan_install`, `install`): re-hashes a copy in a 0700 stage beside the target, then swaps with same-volume
  renames and rolls back on failure. A plain binary comes from the `.tar.xz`/`.zip` (one fixed destination, no entry
  path is used). On macOS the verified `.dmg` is mounted read-only (`hdiutil`), `SQLer.app` is copied out (`ditto`),
  checked (`codesign --verify`, `--version`) and the whole bundle is swapped. Package-manager installs, read-only
  folders and development builds get `Manual` with a reason instead.
- Opt-in: the caller decides whether and when to check. `CheckSchedule` holds the timing (first check at least 30 s
  after the first frame, then every 24 h, backoff from 1 h up to 24 h after failures).
- The asset is an envelope `{format, key_id, signature, payload}`: an Ed25519 signature (verified with `ring`)
  over `SIGNING_CONTEXT` followed by the manifest bytes. The payload is parsed only after it verifies.
- Network access goes only to the configured feed URL (and its redirects: `https`, at most 3). The only request
  header of its own is `User-Agent: SQLer/<version> (update-check)`. No telemetry, no identifiers.
- Configured: the production public key is committed in `keys/` (ADR-0066 note). With an empty key list
  `check()` would return `UpdateError::NotConfigured` without any request.
- Tests use a loopback mock server and TEST-ONLY keys that exist only in `tests/support`; never a release key.

See [AGENTS.md](../../AGENTS.md) for the project handbook and ADR-0066 for the format and policy and ADR-0078 for download and install.
