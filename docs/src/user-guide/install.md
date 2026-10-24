# Install

SQLer 1.0 connects to: SQLite, PostgreSQL, MySQL/MariaDB, SQL Server.

SQL Server is built in to every download as a plugin. Approve the permission `driver.host:mssql` once in the Plugins view, then add a connection (see [SQL Server](databases/sqlserver.md)).

> **Not in 1.0: DuckDB.** The DuckDB driver's code stays in the repository with its `driver-duckdb` feature off by default, and no 1.0 download, installer or update contains it. It may return after 1.0 as a separate download (ADR-0070).

Downloads are on the [GitHub releases page](https://github.com/alwatroos/sqler/releases/latest). The release is built by the pipeline in [ADR-0064](../adr/0064-release-pipeline.md). Builds are unsigned, so macOS and Windows show a warning on first start (see below).

Targets: macOS (Apple silicon and Intel), Windows (x86_64) and Linux (x86_64 and aarch64).

## Files per release

| File | Use |
| ---- | --- |
| `sqler-app-<target>.tar.xz` | Archive for macOS and Linux |
| `sqler-app-x86_64-pc-windows-msvc.zip` | Archive for Windows |
| `sqler-app-x86_64-pc-windows-msvc.msi` | Windows installer |
| `sqler-app-installer.sh` | Shell installer for macOS and Linux |
| `sqler-app-installer.ps1` | PowerShell installer for Windows |
| `<file>.sha256`, `sha256.sum` | SHA-256 checksums, one per file and one combined |
| `sqler-app_<triple>.cdx.xml` | Software bill of materials (CycloneDX), one per target; the Windows `.zip` and `.msi` share one |
| `source.tar.gz`, `dist-manifest.json` | Source archive and the machine-readable list of the release files |

`<target>` is one of `aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-unknown-linux-gnu` or `aarch64-unknown-linux-gnu`.

## macOS and Linux

Shell installer. It puts the binary in `~/.sqler/bin` and writes an `env` script next to it. By default it also appends a line to your shell profile files (for example `.profile`, `.bashrc` and `.bash_profile`) so that `~/.sqler/bin` is on your `PATH`:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/alwatroos/sqler/releases/latest/download/sqler-app-installer.sh | sh
```

To leave your shell profiles alone, set `SQLER_APP_NO_MODIFY_PATH=1` for the installer (`... | SQLER_APP_NO_MODIFY_PATH=1 sh`; the generic `INSTALLER_NO_MODIFY_PATH=1` works too) and add `~/.sqler/bin` to your `PATH` yourself. To uninstall, delete `~/.sqler` and remove the profile line the installer added.

Archive: download `sqler-app-<target>.tar.xz`, unpack it with `tar -xJf` and run the `sqler` binary inside.

### macOS: unsigned build

The app is not signed or notarised, so Gatekeeper blocks the first start. Either open System Settings > Privacy & Security and choose Open Anyway next to the SQLer message, or remove the quarantine flag:

```sh
xattr -dr com.apple.quarantine <path-to-sqler>
```

### Linux: runtime libraries

The window needs the usual desktop libraries on the machine. The release binary (x86_64 and aarch64) is linked against these shared libraries, which the loader needs at start:

- the C runtime: `libc`, `libm` and `libgcc_s`
- X11 client libraries: `libxcb`, `libxcb-xkb`, `libxkbcommon` and `libxkbcommon-x11`, with their dependencies `libXau`, `libXdmcp`, `libbsd` and `libmd`

Libraries that the binary loads at run time instead (a Wayland client library, a Vulkan driver such as `libvulkan1` with a Mesa or vendor driver, `libfontconfig` and `libfreetype`) do not appear in that list. Install them with the desktop; the window needs a graphics driver.

## Windows

PowerShell installer. It puts the binary in `~/.sqler/bin`:

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://github.com/alwatroos/sqler/releases/latest/download/sqler-app-installer.ps1 | iex"
```

By default it also adds `~/.sqler/bin` to your user `PATH`. To prevent that, set `SQLER_APP_NO_MODIFY_PATH=1` (or `INSTALLER_NO_MODIFY_PATH=1`) in the environment first:

```powershell
$env:SQLER_APP_NO_MODIFY_PATH = "1"
powershell -ExecutionPolicy Bypass -c "irm https://github.com/alwatroos/sqler/releases/latest/download/sqler-app-installer.ps1 | iex"
```

Or run the `.msi` installer, or unpack the `.zip` and start `sqler.exe`.

### Windows: SmartScreen

The build is unsigned, so SmartScreen may show "Windows protected your PC". Choose More info, then Run anyway.

## What the installers put on disk

- The shell and PowerShell installers put the `sqler` binary in `~/.sqler/bin`, plus the `env` scripts and the `PATH` line described above.
- The MSI puts `sqler.exe` in `Program Files\sqler-app\bin` and offers an optional `PATH` entry.
- No installer creates `<config>` (the per-OS path is in [Settings](settings.md)) or `<config>/plugins`. That folder is yours: create it when you add a plugin. SQLer creates only `<config>/plugins/<id>/data`, and only for a plugin that reads its data scope. The built-in SQL Server needs no folder.
- Uninstalling (deleting `~/.sqler`, or removing the MSI in Apps) leaves `<config>` alone: your connections, `plugin-grants.toml` and the plugin preferences stay. Delete that folder yourself to remove everything.

### Plugin host

The plugin host runtime (wasmtime and Cranelift) is linked into the binary, so no system library is needed for it.

## Verify a download

Each file has a `.sha256` sidecar, and `sha256.sum` lists all of them. Put the file and its checksum in one directory, then:

```sh
# macOS
shasum -a 256 -c sqler-app-aarch64-apple-darwin.tar.xz.sha256
# Linux
sha256sum -c sqler-app-x86_64-unknown-linux-gnu.tar.xz.sha256
```

```powershell
# Windows: compare the output with the value in the .sha256 file
Get-FileHash -Algorithm SHA256 .\sqler-app-x86_64-pc-windows-msvc.zip
```

## Build from source

You need the Rust toolchain pinned in `rust-toolchain.toml`:

```sh
cargo build --release -p sqler-app --locked
```

The binary is `target/release/sqler`. Linux needs the GPUI build packages listed in `.github/workflows/ci.yml`, and macOS needs the Xcode Metal toolchain. To build what the release ships, add `--features plugins,driver-mssql`.
