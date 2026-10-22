// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The audit sink of the AI surfaces (ADR-0037, ADR-0003 guardrail 5): every tool call is
//! audited, so a surface that can run tools cannot be built without a sink.
//!
//! [`AiAudit`] is a required value the `Workspace` hands to the session, editor and agent jobs.
//! It is the only place that attaches a sink to a tool set or to the MCP server; a source scan
//! below keeps it that way. Without a sink the gates return [`NO_AUDIT`].

use std::sync::Arc;

use sqler_ai_tools::{AuditEntry, AuditSink, Surface, ToolSet};

/// Why the AI surfaces are off when the host has no audit log.
pub const NO_AUDIT: &str = "AI is off: SQLer found no config folder for the AI audit log, and every AI tool call must be audited.";

/// The audit sink an AI surface must have.
#[derive(Clone)]
pub struct AiAudit {
    sink: Arc<dyn AuditSink>,
}

// Prints only its name.
impl std::fmt::Debug for AiAudit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AiAudit")
    }
}

/// Records every entry as one surface into the sink behind it.
struct SurfaceSink {
    inner: Arc<dyn AuditSink>,
    surface: Surface,
}

impl AuditSink for SurfaceSink {
    fn record(&self, entry: &AuditEntry) {
        let mut entry = entry.clone();
        entry.surface = self.surface;
        self.inner.record(&entry);
    }
}

impl AiAudit {
    /// Wraps the sink of the host.
    #[must_use]
    pub fn new(sink: Arc<dyn AuditSink>) -> Self {
        Self { sink }
    }

    /// `tools` with every call audited as `surface`.
    #[must_use]
    #[allow(clippy::needless_pass_by_value)] // the audited set replaces the one given
    pub fn attach(&self, tools: ToolSet, surface: Surface) -> ToolSet {
        tools.with_audit(self.sink.clone(), surface)
    }

    /// A sink that rewrites the surface of each entry, for `McpServer::new`, which forces `mcp`.
    #[must_use]
    pub fn mcp_sink(&self, surface: Surface) -> Arc<dyn AuditSink> {
        Arc::new(SurfaceSink {
            inner: self.sink.clone(),
            surface,
        })
    }

    /// A sink that keeps nothing but the entries in memory: tests and screenshots.
    #[cfg(any(test, feature = "screenshots"))]
    #[must_use]
    pub(crate) fn in_memory() -> Self {
        Self::new(Arc::new(sqler_ai_tools::MemoryAudit::new()))
    }

    /// Whether both wrap the same sink.
    #[must_use]
    pub fn same(&self, other: &AiAudit) -> bool {
        Arc::ptr_eq(&self.sink, &other.sink)
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use sqler_ai_tools::MemoryAudit;

    use super::*;

    fn entry() -> AuditEntry {
        crate::ai::audit::fixture::entries(1_000_000)
            .into_iter()
            .next()
            .expect("the fixture has entries")
    }

    #[test]
    fn mcp_sink_tags_each_surface() {
        for surface in [Surface::Assist, Surface::Session, Surface::Agent] {
            let mem = Arc::new(MemoryAudit::new());
            let audit = AiAudit::new(mem.clone());
            let mut e = entry();
            e.surface = Surface::Mcp;
            audit.mcp_sink(surface).record(&e);
            let got = mem.entries();
            assert_eq!(got.len(), 1);
            assert_eq!(got[0].surface, surface);
        }
    }

    #[test]
    fn attach_keeps_the_sink_and_debug_is_opaque() {
        let mem = Arc::new(MemoryAudit::new());
        let a = AiAudit::new(mem.clone());
        let b = a.clone();
        assert!(a.same(&b));
        assert!(!a.same(&AiAudit::new(Arc::new(MemoryAudit::new()))));
        assert_eq!(format!("{a:?}"), "AiAudit");
    }

    fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
        for item in std::fs::read_dir(dir).expect("read_dir") {
            let path = item.expect("entry").path();
            if path.is_dir() {
                sources(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    /// Nobody builds a sink for a tool set or the MCP server outside the helper.
    #[test]
    fn only_the_helper_attaches_a_sink() {
        // The UI builds the in-app tool sets; the CLI builds the MCP server over stdio.
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut files = Vec::new();
        sources(&manifest.join("src"), &mut files);
        sources(&manifest.join("../sqler-cli/src"), &mut files);
        let norm = |p: &Path| p.to_string_lossy().replace('\\', "/");
        // Built from pieces so this file does not match itself.
        let with_audit = [".with_", "audit("].concat();
        let server_new = ["McpServer", "::new("].concat();
        let null = ["Null", "Audit"].concat();
        let none = [" audit: ", "None"].concat();
        for file in files {
            let name = norm(&file);
            if name.ends_with("workspace/testing.rs") {
                continue;
            }
            let text = std::fs::read_to_string(&file)
                .expect("read")
                .replace("\r\n", "\n");
            // Cut at the test module declaration, so an earlier cfg-gated item hides nothing.
            let prod = text.split("\nmod tests").next().unwrap_or("");
            if !name.ends_with("ai/audit/sink.rs") {
                assert!(!prod.contains(&with_audit), "{name} attaches a sink itself");
            }
            if !name.ends_with("ai/agent/endpoint.rs") && !name.ends_with("sqler-cli/src/mcp.rs") {
                assert!(!prod.contains(&server_new), "{name} builds an MCP server");
            }
            assert!(!prod.contains(&null), "{name} has a null audit");
            assert!(!prod.contains(&none), "{name} has an optional audit");
        }
    }
}
