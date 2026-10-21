# Secrets

SQLer keeps passwords, key passphrases and API keys (database passwords, SSH passwords, SSH key
passphrases, AI provider API keys) in
your operating system's keychain. They are never written to `connections.toml` or any other SQLer
file. A connection only stores a reference such as `my-db:db-password`.

## Where they live

| OS | Store | How to find an entry |
| -- | ----- | -------------------- |
| macOS | Keychain | Keychain Access, item name and service `SQLer`, account `<connection-id>:<kind>` |
| Windows | Credential Manager | Windows Credentials, an entry whose name contains `SQLer` and `<connection-id>:<kind>` |
| Linux | Secret Service (GNOME Keyring, KWallet and others) | Seahorse or KWalletManager, attribute service `SQLer` |

The kind is `db-password`, `ssh-password` or `ssh-key-passphrase`; for an AI provider key it is `ai-provider-key`.

## Provider API keys

The API key of an AI provider (see [AI](ai.md)) is stored the same way. Its account is
`<provider-id>:ai-provider-key`, for example `local:ai-provider-key`, under the service `SQLer`.
`ai.toml` holds only `api_key_in_keychain = true`. Because the account uses the provider `id`, keep the
`id` when you edit the file by hand.

## Locked or missing keychain

If the keychain is locked, access is denied, or no Secret Service is running (for example on a
headless Linux machine), SQLer reports an error that names the entry, such as
`keychain unavailable for my-db:db-password: ...`. Nothing is saved in plain text and SQLer does not
switch to another store. Unlock or start the keychain and try again. On Linux, restart SQLer after
starting the Secret Service: the connection to it is set up once per process, so a missing service at
the first use keeps failing until SQLer restarts.

## Removing entries by hand

Delete the entries whose service is `SQLer` in the keychain tool listed above. SQLer treats a missing
entry as "no password stored".
