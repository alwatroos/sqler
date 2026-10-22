// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Probe fixture plugin (`ui-plugin` world). Commands misbehave on request and status items report what the sandbox
//! lets the guest see; the host tests (`crates/sqler-plugin-host/tests/guests.rs`) drive both.

// The wit-bindgen macro expands to `unsafe` FFI glue and undocumented items (AGENTS.md section 4 exception),
// so the generated code and the exports live in one module with the lints relaxed.
#[allow(unsafe_code, missing_docs, clippy::all, clippy::pedantic)]
mod bindings {
    wit_bindgen::generate!({
        path: "../../../crates/sqler-plugin-api/wit",
        world: "ui-plugin",
    });

    use std::fmt::Write as _;
    use std::time::Duration;

    use self::exports::sqler::plugin::lifecycle;
    use self::exports::sqler::plugin::ui_handler::{
        CommandContext, Guest, PluginError, WidgetEvent, WidgetTree,
    };
    use self::sqler::plugin::log::{Level, write};
    use self::sqler::plugin::settings::{self, SettingValue};
    use self::sqler::plugin::types::ErrorCode;

    struct Probe;

    impl lifecycle::Guest for Probe {
        fn init() -> Result<(), lifecycle::PluginError> {
            write(Level::Info, "probe: init");
            Ok(())
        }
    }

    impl Guest for Probe {
        fn run_command(command: String, _context: CommandContext) -> Result<(), PluginError> {
            match command.as_str() {
                "ok" => Ok(()),
                "spin" => loop {
                    std::hint::black_box(());
                },
                "alloc" => {
                    // Grows by 1 MiB until the host refuses `memory.grow` and the allocator aborts.
                    let mut hoard: Vec<Vec<u8>> = Vec::new();
                    loop {
                        hoard.push(vec![1_u8; 1 << 20]);
                        std::hint::black_box(&hoard);
                    }
                }
                "panic" => panic!("probe: deliberate panic"),
                "trap" => {
                    core::arch::wasm32::unreachable();
                }
                "sleep" => {
                    std::thread::sleep(Duration::from_secs(3600));
                    Ok(())
                }
                "log-flood" => {
                    for i in 0..1000 {
                        write(Level::Info, &format!("flood {i}"));
                    }
                    Ok(())
                }
                other => Err(PluginError {
                    code: ErrorCode::NotFound,
                    message: format!("unknown command {other}"),
                }),
            }
        }

        fn render_panel(_panel: String) -> WidgetTree {
            // The probe declares no panels; the host never asks.
            WidgetTree { nodes: Vec::new() }
        }

        fn panel_event(_panel: String, _event: WidgetEvent) -> Option<WidgetTree> {
            None
        }

        fn status(item: String) -> Option<String> {
            let text = match item.as_str() {
                "env" => format!(
                    "vars={} args={}",
                    std::env::vars().count(),
                    std::env::args().count()
                ),
                "read-data" => read("/data/hello.txt"),
                "read-plugin" => read("/plugin/sqler.toml"),
                // A `..` that leaves the preopen must be refused even though the target exists.
                "read-escape" => read("/data/../escape.txt"),
                // Symlinks inside the granted scope that point out of it must be refused too.
                "read-link" => read("/data/link.txt"),
                "read-linkdir" => read("/data/up/escape.txt"),
                // Outside every preopen.
                "read-root" => read("/etc/passwd"),
                "setting" => url(),
                "http" => fetch(&url()),
                "http-other" => fetch(&other()),
                _ => return None,
            };
            Some(text)
        }
    }

    fn read(path: &str) -> String {
        match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) => format!("err:{:?}", e.kind()),
        }
    }

    fn url() -> String {
        match settings::get("url") {
            Some(SettingValue::Text(t)) => t,
            _ => String::new(),
        }
    }

    /// A host the grant never names, on the port of the configured URL.
    fn other() -> String {
        let u = url();
        let port = authority(&u)
            .and_then(|a| a.rsplit_once(':').map(|(_, p)| p.to_owned()))
            .unwrap_or_else(|| "80".to_owned());
        format!("http://denied.invalid:{port}/")
    }

    /// `http://host:port/path` -> `host:port`.
    fn authority(url: &str) -> Option<&str> {
        let rest = url.strip_prefix("http://")?;
        Some(rest.split('/').next().unwrap_or(rest))
    }

    fn fetch(url: &str) -> String {
        use wasip2::http::outgoing_handler;
        use wasip2::http::types::{Fields, Method, OutgoingRequest, Scheme};

        let Some(auth) = authority(url) else {
            return "err:no url".to_owned();
        };
        let path = url
            .strip_prefix("http://")
            .and_then(|r| r.find('/').map(|i| r[i..].to_owned()))
            .unwrap_or_else(|| "/".to_owned());
        let request = OutgoingRequest::new(Fields::new());
        if request.set_method(&Method::Get).is_err()
            || request.set_scheme(Some(&Scheme::Http)).is_err()
            || request.set_authority(Some(auth)).is_err()
            || request.set_path_with_query(Some(&path)).is_err()
        {
            return "err:bad request".to_owned();
        }
        let future = match outgoing_handler::handle(request, None) {
            Ok(f) => f,
            Err(e) => return format!("err:{e}"),
        };
        future.subscribe().block();
        let mut out = String::new();
        match future.get() {
            Some(Ok(Ok(response))) => {
                let _ = write!(out, "status={}", response.status());
            }
            Some(Ok(Err(e))) => {
                let _ = write!(out, "err:{e}");
            }
            _ => out.push_str("err:no response"),
        }
        out
    }

    export!(Probe);
}
