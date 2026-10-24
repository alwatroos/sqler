// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Applying the plan: the connections go into the live store, the typed secrets are only recorded.

use std::sync::Mutex;

use sqler_config::{Secret, SecretError, SecretRef, SecretStore};
use sqler_import::{PlanAction, ProvidedSecret};

use super::{ImportStep, ImportSummary, ImportWizard};
use crate::sidebar::SidebarModel;
use crate::workspace::SecretOp;

/// A [`SecretStore`] that writes nothing: `set` records the call, so the keychain is touched only
/// after `connections.toml` was saved (`Workspace::commit_connections`).
#[derive(Default)]
struct Recorder {
    sets: Mutex<Vec<(SecretRef, Secret)>>,
}

impl Recorder {
    fn into_ops(self) -> Vec<SecretOp> {
        self.sets
            .into_inner()
            .unwrap_or_default()
            .into_iter()
            .map(|(r, s)| SecretOp::Set(r, s))
            .collect()
    }
}

impl SecretStore for Recorder {
    fn set(&self, r: &SecretRef, secret: &Secret) -> Result<(), SecretError> {
        if let Ok(mut sets) = self.sets.lock() {
            sets.push((r.clone(), secret.clone()));
        }
        Ok(())
    }

    fn get(&self, _: &SecretRef) -> Result<Option<Secret>, SecretError> {
        Ok(None)
    }

    fn delete(&self, _: &SecretRef) -> Result<bool, SecretError> {
        Ok(false)
    }
}

impl ImportWizard {
    /// Adds the selected connections to the sidebar's store and moves to the summary. Returns the
    /// keychain writes to make once the connections were saved.
    pub(crate) fn apply(&mut self, sidebar: &mut SidebarModel) -> Vec<SecretOp> {
        let Some(original) = self.plan.as_ref() else {
            return Vec::new();
        };
        let total = original.items.len();
        let mut plan = original.clone();
        plan.items.retain(|i| self.selected.contains(&i.index));
        let provided: Vec<ProvidedSecret> = self
            .secrets
            .iter()
            .filter(|((index, _), _)| self.selected.contains(index))
            .map(|((index, kind), secret)| ProvidedSecret {
                index: *index,
                kind: *kind,
                secret: secret.clone(),
            })
            .collect();
        let recorder = Recorder::default();
        let report =
            sidebar.import_with(|store| sqler_import::apply(&plan, store, &recorder, provided));
        let name_of = |index: usize| {
            plan.items
                .iter()
                .find(|i| i.index == index)
                .map(|i| match &i.action {
                    PlanAction::Add { spec, .. } => spec.name.clone(),
                    PlanAction::Skip(_) => i.foreign_name.clone(),
                })
                .unwrap_or_default()
        };
        self.summary = Some(ImportSummary {
            added: report.added.iter().map(|(i, _)| name_of(*i)).collect(),
            not_imported: total.saturating_sub(report.added.len()),
            passwords_stored: report.secrets_stored.len(),
            passwords_skipped: report.secrets_skipped.len(),
            failed: report
                .failed
                .iter()
                .map(|(i, text)| (name_of(*i), text.clone()))
                .collect(),
        });
        self.secrets.clear();
        self.step = ImportStep::Summary;
        self.cursor = 0;
        recorder.into_ops()
    }
}
