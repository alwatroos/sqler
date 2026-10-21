// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The UI host: what only an app with a window can do.

use sqler_ai_api::BoxFuture;

use crate::error::ToolError;

/// Implemented by hosts that have an editor and result tabs. Headless hosts (MCP, CLI) give
/// the tool set none, and then `open_in_editor` and `open_results_tab` are neither listed nor callable.
pub trait UiHost: Send + Sync {
    /// Puts `sql` into an editor tab, on `connection` if given. Does not run it.
    fn open_in_editor<'a>(
        &'a self,
        connection: Option<&'a str>,
        sql: &'a str,
    ) -> BoxFuture<'a, Result<(), ToolError>>;

    /// Opens a results tab that shows the (read-only) `sql` on `connection`.
    fn open_results_tab<'a>(
        &'a self,
        connection: &'a str,
        sql: &'a str,
        title: Option<&'a str>,
    ) -> BoxFuture<'a, Result<(), ToolError>>;
}
