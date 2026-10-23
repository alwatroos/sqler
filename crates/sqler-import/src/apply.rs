// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Applies a plan to an in-memory store and the secret store.

use sqler_config::{
    ConnectionId, ConnectionSpec, ConnectionStore, GroupId, ProfileSecretKind, SecretRef,
    SecretStore, SshAuth,
};

use crate::model::{ApplyReport, ImportPlan, PlanAction, PlanItem, ProvidedSecret};

/// Adds the `Add` items of `plan` to `store` and writes the secrets the user typed in.
///
/// Only new connections and groups are created: an existing group of the same name under the same
/// parent is reused, and no existing connection or group is renamed, moved or changed. A secret is
/// stored only when a [`ProvidedSecret`] matches an item and a kind that item lists; the matching
/// flag in the connection (`password_in_keychain`, `passphrase_in_keychain`) is set only then, and
/// cleared again when the secret store refuses the write. Provided secrets for skipped or unknown
/// items are dropped.
///
/// The caller saves the store (`ConfigDir::save_connection_store`). This call blocks on the keychain,
/// so async callers run it with `spawn_blocking`.
#[must_use]
#[allow(clippy::needless_pass_by_value)] // owns the secrets so they are dropped when the call ends
pub fn apply(
    plan: &ImportPlan,
    store: &mut ConnectionStore,
    secrets: &dyn SecretStore,
    provided: Vec<ProvidedSecret>,
) -> ApplyReport {
    let mut report = ApplyReport::default();
    for item in &plan.items {
        let PlanAction::Add { spec, .. } = &item.action else {
            continue;
        };
        let given = |kind: ProfileSecretKind| {
            item.secrets.contains(&kind)
                && provided.iter().any(|p| {
                    p.index == item.index && p.kind == kind && !p.secret.expose().is_empty()
                })
        };
        let mut spec = spec.clone();
        spec.password_in_keychain = given(ProfileSecretKind::DbPassword);
        if let Some(ssh) = &mut spec.ssh
            && let SshAuth::KeyFile {
                passphrase_in_keychain,
                ..
            } = &mut ssh.auth
        {
            *passphrase_in_keychain = given(ProfileSecretKind::SshPassphrase);
        }
        let id = match add_item(store, item, spec.clone()) {
            Ok(id) => id,
            Err(text) => {
                report.failed.push((item.index, text));
                continue;
            }
        };
        report.added.push((item.index, id.clone()));
        store_secrets(store, secrets, &provided, item, &id, spec, &mut report);
    }
    report
}

/// Adds the connection at the root, then moves it into its group, so a spec error leaves no group behind.
fn add_item(
    store: &mut ConnectionStore,
    item: &PlanItem,
    spec: ConnectionSpec,
) -> Result<ConnectionId, String> {
    let id = store
        .add_connection(spec, None)
        .map_err(|e| e.to_string())?;
    if item.group.is_empty() {
        return Ok(id);
    }
    let moved = resolve_group(store, &item.group).and_then(|group| {
        store
            .move_connection(&id, Some(&group), usize::MAX)
            .map_err(|e| e.to_string())
    });
    match moved {
        Ok(()) => Ok(id),
        Err(text) => {
            // Roll back so the connection is not left at the root unannounced.
            let _ = store.delete_connection(&id);
            Err(text)
        }
    }
}

fn resolve_group(store: &mut ConnectionStore, path: &[String]) -> Result<GroupId, String> {
    let mut parent: Option<GroupId> = None;
    for name in path {
        let found = store
            .child_groups(parent.as_ref())
            .find(|g| g.name() == name)
            .map(|g| g.id().clone());
        let id = match found {
            Some(id) => id,
            None => store
                .add_group(name, parent.as_ref())
                .map_err(|e| e.to_string())?,
        };
        parent = Some(id);
    }
    parent.ok_or_else(|| "empty group path".to_owned())
}

fn store_secrets(
    store: &mut ConnectionStore,
    secrets: &dyn SecretStore,
    provided: &[ProvidedSecret],
    item: &PlanItem,
    id: &ConnectionId,
    spec: ConnectionSpec,
    report: &mut ApplyReport,
) {
    let mut spec = spec;
    for &kind in &item.secrets {
        let value = provided
            .iter()
            .find(|p| p.index == item.index && p.kind == kind && !p.secret.expose().is_empty());
        let Some(value) = value else {
            report.secrets_skipped.push((item.index, kind));
            continue;
        };
        let secret_ref = store
            .connection(id)
            .and_then(|c| c.secret_refs().into_iter().find(|r| r.kind() == kind))
            .and_then(|r| SecretRef::try_from(&r).ok());
        let stored = secret_ref.is_some_and(|r| secrets.set(&r, &value.secret).is_ok());
        if stored {
            report.secrets_stored.push((item.index, kind));
            continue;
        }
        report.secrets_skipped.push((item.index, kind));
        match kind {
            ProfileSecretKind::DbPassword => spec.password_in_keychain = false,
            ProfileSecretKind::SshPassphrase => {
                if let Some(SshAuth::KeyFile {
                    passphrase_in_keychain,
                    ..
                }) = spec.ssh.as_mut().map(|s| &mut s.auth)
                {
                    *passphrase_in_keychain = false;
                }
            }
            _ => {}
        }
        if store.update_connection(id, spec.clone()).is_err() {
            report
                .failed
                .push((item.index, "could not clear the keychain flag".to_owned()));
        }
    }
}
