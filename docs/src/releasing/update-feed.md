# Update feed

The update check (ADR-0066) reads one signed file, `sqler-update.json`, from the newest GitHub Release. This page
records the signing key, the feed, the release jobs that produce the file, how to check it by hand and how to rotate
the key.

## Signing key

- Algorithm: Ed25519.
- Key id: `efe45a1253fd9347`.
- Public key file: `crates/sqler-update/keys/efe45a1253fd9347.pub`.
- Fingerprint (SHA-256 of the raw 32-byte public key): `efe45a1253fd934792dea0989d12ce98ed74acea6acb8832fa00362341fc394b`.
- Created: 2026-10-23.
- Private key: the environment secret `SQLER_UPDATE_SIGNING_KEY` of the GitHub environment `release`. It holds one
  line, the standard base64 of the 32-byte Ed25519 seed. It is not in the repository and cannot be read back from
  GitHub.

To check a build against this page, compute the SHA-256 of the base64-decoded content of the key file and compare
it with the fingerprint above; its first 8 bytes are the key id.

## Feed

The client reads `https://github.com/alwatroos/sqler/releases/latest/download/sqler-update.json`. GitHub serves
`latest` as the newest release that is neither a draft nor a pre-release, so a pre-release never reaches the feed.
The file is one JSON envelope (`format`, `key_id`, `signature`, `payload`); `payload` is the base64 of the manifest.

## Manifest

| Field | Source |
| ----- | ------ |
| `schema`, `product` | fixed: `1`, `sqler` |
| `version` | `app_version` of the `sqler-app` release in the cargo-dist plan manifest |
| `published` | UTC time of the job, `YYYY-MM-DDTHH:MM:SSZ` (informational) |
| `notes_url` | `https://github.com/alwatroos/sqler/releases/tag/<tag>` |
| `platforms` | one entry per target triple of `dist-workspace.toml` |

A platform entry has `size` and `sha256` (computed over the final archive, after macOS signing, and checked against
the `.sha256` sidecar) and `url`, which is the pinned tag URL `releases/download/<tag>/<archive>`. The archive is the
`.tar.xz`, or the `.zip` on Windows (not the `.msi`). The `DuckDB` engine is not part of any archive and never in the
manifest.

## Release jobs

Both jobs are in `.github/workflows/release.yml` and call `xtask release manifest`.

- `update manifest` runs on `v*` tag runs only, in the GitHub environment `release`, after the build and the macOS
  signing step. It builds `xtask` first, then writes the secret `SQLER_UPDATE_SIGNING_KEY` to a file readable by its
  owner only in the runner temp directory, removes it on exit and runs `xtask release manifest --key-file`. The
  secret is one line, the standard base64 of the 32-byte Ed25519 seed. It then runs `xtask release verify` on the
  output (an independent check against the compiled-in key), and uploads the Actions artifact `update-manifest`.
  The `host` job downloads it and attaches `sqler-update.json` in the same `gh release create` as the archives, then
  checks that the asset is listed. So the feed never points at a release without the file.
- If the secret is absent, the job prints a notice and signs nothing: the tag run still publishes the release, without
  a manifest. Clients then keep the previous release as their feed until a signed one exists. A secret that is set but
  wrong (not compiled in, malformed) fails the job and nothing publishes.
- `update manifest (dry run)` runs on pull requests and manual dispatches. It has no environment and no secret. It
  generates a throwaway key, signs with `--test-key` and uploads the Actions artifact `update-manifest-TEST-ONLY`
  holding `sqler-update.TEST-ONLY.json`. That key is not compiled in, so the file is TEST ONLY: it must never be
  attached to a release, and no client accepts it. The artifact is never named `artifacts-*`, so cargo-dist does not
  publish it.

## Verify by hand

1. Download the feed: `curl -fsSLo sqler-update.json https://github.com/alwatroos/sqler/releases/latest/download/sqler-update.json`.
2. Check the signature against the keys compiled into the source tree: `cargo run -p xtask -- release verify sqler-update.json`.
   It prints `ok key_id=...`, the version and one line per platform (`<triple> <size> <sha256> <url>`). Use
   `--public-key-file crates/sqler-update/keys/<key id>.pub` to check against one key file.
3. Read the manifest itself: `jq -r .payload sqler-update.json | base64 -d | jq .`.
4. Download an archive from its `url` and compare: `shasum -a 256 <archive>` must equal that entry's `sha256`, and the
   file size must equal `size`.

## Rotation

Documentation only; this describes the order, it does not rotate anything. `xtask release manifest` refuses a signing
key whose public half is not compiled into `sqler-update`, so the order matters (ADR-0066).

1. Commit the new `.pub` in `crates/sqler-update/keys/` and add it to the `EMBEDDED` list. It ships in release N,
   which is still signed with the old key.
2. After release N exists, replace the value of the secret `SQLER_UPDATE_SIGNING_KEY` and tag release N+1, which is
   signed with the new key. Clients of release N already trust both keys. (Landing the new `.pub` and the secret swap
   before the same tag also works, because the tagged tree then contains the key.)
3. In release N+2, remove the old key.
4. After each release run `cargo run -p xtask -- release verify` on the downloaded feed.

If the secret is swapped before the new `.pub` is in the tagged tree, the tag run fails closed and nothing publishes;
fix the tree or the secret and tag again.
