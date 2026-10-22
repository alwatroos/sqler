# Plugins

## Permissions (grants)

A plugin can do nothing until you allow it. This build has the permission store and the permission dialog as a
component; plugins cannot be installed or run yet, and the plugin manager that shows the dialog and offers revoking
(P7) is not part of it. Once it is, SQLer shows the dialog, titled **Allow plugin {name}?** with one row per permission
the plugin asks for, when it first meets a plugin or a changed one.

![The permission dialog in the dark theme](../screenshots/after/plugins-grant-dark.png)

![The permission dialog in the light theme](../screenshots/after/plugins-grant-light.png)

### What the dialog shows

| Permission | Text | Extra |
| ---------- | ---- | ----- |
| `db.read` | Read the catalog and run read-only queries on your connections | |
| `db.write` | Ask to change data or schema on your connections | Highlighted. Every change goes through the same confirmation as your own statements. |
| `ui.panel` | Show its own panels | |
| `ai.tool` | Offer tools to the AI assistant | Highlighted. |
| `driver.guest` | Provide a database driver that runs inside the plugin | Highlighted. |
| `driver.host:<engine>` | Use a built-in database engine | Highlighted, with the engine name. |
| `net.http:<host>` | Connect to a web host | Highlighted. The host pattern exactly as declared, with its port ("ports 80 and 443" or "port N"). |
| `fs.read:plugin`, `fs.read:data...` | Read files in its own install folder or its own data folder | Highlighted, with the folder scope. |
| `fs.read:home/...` | Read files in your home folder | Highlighted, with the folder scope. |

Only `db.read` and `ui.panel` are shown quietly. Every other permission is highlighted, whatever host or folder it names.

Warnings you should read:

- **Wildcard hosts** (`*.example.com`) include every subdomain.
- **Punycode hosts** (a label starting `xn--`) are international names shown in their encoded form, never decoded.
  Check them carefully; they can imitate another site.
- **`localhost`** and every `*.localhost` name are your own computer.
- **Home folder scopes**: under `Library` or `AppData` they hold the settings and data of other
  applications.

At the top the dialog shows a summary ("N permissions, M need attention"; "none need attention" appears only when no
permission is highlighted) and, as a chip, every highlighted permission with its host or folder; the full list below scrolls and has a scrollbar. Highlighted rows are listed first. When the dialog opens because the plugin changed, capabilities that were not in your
previous approval are marked as new. Names and descriptions come from the plugin; control and direction-changing
characters, invisible format characters and line separators are replaced and long text is cut.

### Answering

- **Approve** allows exactly the permissions listed. There is no partial approval. It answers only to a mouse (or
  touch) click: no key is bound to it, it is skipped by Tab, and Enter or Space on it does nothing.
- **Deny** (or **Escape**, pressed anywhere in the dialog) allows nothing. The answer is remembered and the plugin still loads without permissions.

Known limitation: with the keyboard alone you can read the dialog and Deny, but you cannot Approve. A keyboard way to
confirm (typing the plugin id) is planned before the dialog is reachable.

If another SQLer window denied or revoked the same plugin while the dialog was open, your answer is not saved and the
dialog is shown again with the current state.

The commands are `plugins:approve-grant` ("Approve Plugin Permissions") and `plugins:deny-grant` ("Deny Plugin
Permissions").

### When SQLer asks again

An answer is tied to the plugin id, its version, its manifest and its component file
(`id@version#manifest_hash+component_hash`). Any change to one of them, such as an update, an edited manifest or a
replaced component, means a new question. Only the latest answer per plugin is kept.

### Revoking

The store can revoke an approval, which turns it into a denial for the plugin's current version, so SQLer does not
ask again at the next start. There is no screen for this in this build; the plugin manager (P7) will add the revoke
action and a way to be asked again.

### The file

Answers are stored in `plugin-grants.toml` in the [configuration directory](settings.md) (version 1, one `[[grant]]` per
plugin with `key`, `decision` and `capabilities`). On Unix it is written with mode 0600.

If the file cannot be trusted, SQLer denies everything and does not rewrite it. This applies when it is not valid TOML,
has unknown fields, is larger than 1 MiB, has more than 1024 records, comes from a newer SQLer, cannot be read, or (on
Unix) is writable by other users. The dialog then shows the problem and has no Approve button. Fix or delete the file to
continue. Anyone who can write to your configuration directory can add grants, as they can add connections.
