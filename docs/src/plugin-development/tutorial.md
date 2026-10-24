# Tutorial: your first plugin

This tutorial takes you from a checkout to a plugin that runs in SQLer. You generate a crate from the
template, build it to a WebAssembly component, place it in the plugin directory, approve it and run its command. At the end
you add a capability and a panel.

The plugin is a `ui-plugin` world guest: one command (`Say hello`) and one status item. It asks for no
capability. The example [`plugins/hello-panel`](https://github.com/alwatroos/sqler/tree/main/plugins/hello-panel) is the
same shape with a panel.

## Prerequisites

- A checkout of the SQLer repository. All commands below run from its root unless a step says otherwise.
- [rustup](https://rustup.rs/). The repository's `rust-toolchain.toml` selects the Rust version and the
  `wasm32-wasip2` target, so the first `cargo` command in the checkout installs them.
- `cargo-generate` 0.25.1, the version this tutorial was checked with (the install compiles it and takes a few minutes):

```sh
cargo install cargo-generate --version 0.25.1 --locked
```

- A SQLer build with plugin support. Release builds have it; a local `cargo build` needs `--features plugins`
  ([Plugins](../user-guide/plugins.md)). You build it in step 5.

## Try the example first

Build the example plugin and start SQLer with it as a development plugin. This shows what you are going to make.

```sh
cargo run -p xtask -- plugins build hello-panel
```

```sh
cargo run --release -p sqler-app --features plugins -- --plugin-dir target/plugins/debug/hello-panel
```

SQLer asks you to approve the plugin (see step 6). The command is in the palette (**Mod+Shift+P**) as
`Hello Panel (plugin): Say hello`. Close SQLer before you go on.

## 1. Generate the crate

The template lives in `plugins/template`. Generate your plugin next to the checkout, not inside it:

```sh
cargo generate --path plugins/template --name my-first-plugin --define display_name="My first plugin" --destination ..
```

This creates `../my-first-plugin`. The name you give is the plugin id. It must match
`[a-z][a-z0-9]*(-[a-z0-9]+)*` (see [Manifest](manifest.md)). The generated crate is its own cargo workspace and carries
a copy of the WIT package, the toolchain file and the release profile, so it builds anywhere.

The files that matter:

| File | What it is |
| ---- | ---------- |
| `sqler.toml` | The manifest: id, name, version, `api`, the capability list and the contributions. |
| `src/lib.rs` | The guest: the exported functions of the `ui-plugin` world. |
| `wit/` | The WIT package `sqler:plugin`. Do not edit it. |
| `Cargo.toml` | A `cdylib` crate that depends on `wit-bindgen`. |

The manifest declares `capabilities = []`, one command `say-hello` and one status item `greetings`. The
component entry is `my_first_plugin.wasm`, the name cargo gives the build output.

## 2. Build it

```sh
cd ../my-first-plugin
```

```sh
cargo build --release --target wasm32-wasip2
```

The component is `target/wasm32-wasip2/release/my_first_plugin.wasm`. A cargo build of a `cdylib` for this target is
already a component, so no extra tool is needed.

## 3. Place it in the plugin directory

SQLer looks for plugins in the `plugins` folder of its [configuration directory](../user-guide/settings.md). Each plugin
is a folder named after its id that holds `sqler.toml` and the component. Copy those two files.

macOS:

```sh
mkdir -p "$HOME/Library/Application Support/SQLer/plugins/my-first-plugin"
cp sqler.toml target/wasm32-wasip2/release/my_first_plugin.wasm "$HOME/Library/Application Support/SQLer/plugins/my-first-plugin/"
```

Linux (if `XDG_CONFIG_HOME` is set, use it instead of `~/.config`):

```sh
mkdir -p "$HOME/.config/sqler/plugins/my-first-plugin"
cp sqler.toml target/wasm32-wasip2/release/my_first_plugin.wasm "$HOME/.config/sqler/plugins/my-first-plugin/"
```

Windows (PowerShell):

```powershell
New-Item -ItemType Directory -Force "$env:APPDATA\SQLer\plugins\my-first-plugin"
Copy-Item sqler.toml, target\wasm32-wasip2\release\my_first_plugin.wasm "$env:APPDATA\SQLer\plugins\my-first-plugin\"
```

While you develop you can skip the copy and start SQLer with `--plugin-dir` on a folder that holds both files (see
[Publishing](publishing.md)).

## 4. Rebuild after a change

After every change to `src/lib.rs` or `sqler.toml`, build again and copy the two files again. SQLer reads the plugin when
it starts, or when you choose **Rescan** in the Plugins view.

## 5. Start SQLer

Go back to the checkout root (the folder you ran `cargo generate` from) and start the app:

```sh
cargo run --release -p sqler-app --features plugins
```

## 6. Approve the plugin

The first time SQLer meets a plugin that is switched on, it shows the permission dialog. Click **Approve** with the mouse,
or type the plugin id (`my-first-plugin`) into the confirmation field and press **Enter**. Nothing else approves. The
details are in [Plugins](../user-guide/plugins.md). The list of the plugin's permissions is empty, because the manifest
asks for none.

Open the **Plugins** view to check that `My first plugin` is listed and switched on.

## 7. Run the command

Open the command palette (**Mod+Shift+P**) and run:

```text
My first plugin (plugin): Say hello
```

SQLer shows the notification "Hello from a plugin!" and the status bar shows `Hellos: 1`. Run it again to count up.

## 8. Add a capability: a panel

A plugin can only do what its manifest declares and you approve. To show a panel, the plugin needs `ui.panel`.

In `sqler.toml` change the capability list and add the panel:

```toml
capabilities = ["ui.panel"]
```

```toml
[[panels]]
id = "hello"
title = "Hello"
```

In `src/lib.rs`, add `Stack`, `Direction`, `Gap`, `Text`, `TextStyle`, `Widget`, `WidgetNode` and `WidgetTree` to the
imports as `plugins/hello-panel/src/lib.rs` does, and make `render_panel` return a small tree: a vertical stack whose
only child is a text. Adapt the `tree` function of `hello-panel`:

```rust
fn render_panel(_panel: String) -> WidgetTree {
    WidgetTree {
        nodes: vec![
            WidgetNode {
                widget: Widget::Stack(Stack { direction: Direction::Vertical, gap: Gap::Medium }),
                children: vec![1],
            },
            WidgetNode {
                widget: Widget::Text(Text { content: "Hello from my panel".to_owned(), style: TextStyle::Body }),
                children: Vec::new(),
            },
        ],
    }
}
```

Rebuild and copy (steps 2 and 3), then restart SQLer or choose **Rescan**. The manifest changed, so SQLer asks again and the
dialog now lists `ui.panel` ([When SQLer asks again](../user-guide/plugins.md#when-sqler-asks-again)). Approve it, then
run the palette command:

```text
View: Show Hello (My first plugin)
```

The panel appears in the **Plugins** bottom tab. For buttons and click events see `plugins/hello-panel/src/lib.rs`.

## Troubleshooting

- **The build says the target is missing.** Run the build from inside the generated crate, or add the target:
  `rustup target add wasm32-wasip2`.
- **The plugin is under "Could not load".** The reason is shown next to it. Common reasons: `sqler.toml` is not valid,
  the `[entry]` file is not in the folder, or the folder name is not the plugin id.
- **The id is refused.** It must match `[a-z][a-z0-9]*(-[a-z0-9]+)*` and must not be a reserved Windows name such as
  `con`, `nul`, `aux` or `prn`.
- **The plugin is not listed at all.** Check that your SQLer build has the `plugins` feature (release builds do; a local build needs `--features plugins`; the Plugins view would say
  "This build has no plugin support").
- **A command does nothing.** The plugin may be switched off, failed or not approved. Contributions of such a plugin are
  removed ([Plugins](../user-guide/plugins.md)).

## Next steps

- Add an export file format: [Exporters](exporters.md) and the example `plugins/exporter-markdown`.
- Add completion rows to the SQL editor: [Completion providers](completion.md).
- Provide a database driver: [Drivers](drivers.md).
- Offer tools to the AI session: [Capabilities](capabilities.md) and
  [ADR-0062](../adr/0062-plugin-ai-tools.md).
- More UI contributions (menus, panels, status items):
  [ADR-0060](../adr/0060-plugins-in-the-workbench.md).
- Every WIT function and type: [WIT reference](wit-reference.md).
