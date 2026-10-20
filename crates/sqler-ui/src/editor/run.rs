// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The seam between the editor and query execution.

use crate::tabs::TabId;
use crate::workspace::{Effect, Workspace};

use super::EditorBuffer;

/// What a run command executes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RunMode {
    /// The selected text.
    Selection,
    /// The statement at the cursor.
    Statement,
    /// The whole buffer.
    All,
}

/// Starts and cancels runs for an editor tab. The real adapter over the query engine lives apart so the editor
/// can be tested with a fake.
pub trait RunPort {
    /// Runs `buffer` of editor `tab` in `mode`.
    ///
    /// # Errors
    ///
    /// A message for the user when the run cannot start.
    fn run(
        &mut self,
        ws: &mut Workspace,
        tab: TabId,
        mode: RunMode,
        buffer: EditorBuffer,
    ) -> Result<Effect, String>;

    /// Cancels the run of editor `tab`.
    ///
    /// # Errors
    ///
    /// A message for the user when there is nothing to cancel or it fails.
    fn cancel(&mut self, ws: &mut Workspace, tab: TabId) -> Result<Effect, String>;
}

/// A [`RunPort`] that records its calls and does nothing.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct FakeRunPort {
    /// Each call: the tab, the mode (`None` for a cancel) and the buffer (`None` for a cancel).
    pub calls: Vec<(TabId, Option<RunMode>, Option<EditorBuffer>)>,
}

#[cfg(test)]
impl RunPort for FakeRunPort {
    fn run(
        &mut self,
        _ws: &mut Workspace,
        tab: TabId,
        mode: RunMode,
        buffer: EditorBuffer,
    ) -> Result<Effect, String> {
        self.calls.push((tab, Some(mode), Some(buffer)));
        Ok(Effect::None)
    }

    fn cancel(&mut self, _ws: &mut Workspace, tab: TabId) -> Result<Effect, String> {
        self.calls.push((tab, None, None));
        Ok(Effect::None)
    }
}

#[cfg(test)]
mod tests {
    use sqler_driver_api::sqler_core::DriverId;

    use super::*;
    use crate::sidebar::SidebarModel;
    use crate::tabs::{TabConnection, TabKind, TabStrip};

    #[test]
    fn the_fake_records_runs_and_cancels() {
        let mut strip = TabStrip::default();
        let conn = TabConnection {
            name: "c".to_owned(),
            driver: DriverId::new("sqlite").unwrap(),
            color: None,
            environment: None,
        };
        let tab = strip.open(conn, TabKind::Query);
        let mut ws = Workspace::new(SidebarModel::default());
        let mut port = FakeRunPort::default();
        let buffer = EditorBuffer::new("select 1", 8);
        let r = port.run(&mut ws, tab, RunMode::Statement, buffer.clone());
        assert!(matches!(r, Ok(Effect::None)));
        assert!(matches!(port.cancel(&mut ws, tab), Ok(Effect::None)));
        assert_eq!(
            port.calls,
            vec![
                (tab, Some(RunMode::Statement), Some(buffer)),
                (tab, None, None)
            ]
        );
    }
}
