// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The dry run: checks a report against the store and the available drivers.

use std::collections::HashSet;

use sqler_config::{ConnectionId, ConnectionSpec, ConnectionStore};

use crate::MAX_GROUP_DEPTH;
use crate::model::{
    EntryOutcome, ImportOptions, ImportPlan, PlanAction, PlanItem, SkipReason, SourceEntry,
    SourceReport,
};

/// What makes two connections the same server: driver, target and SSH host, port and user.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Key {
    driver: String,
    target: String,
    ssh: Option<(String, u16, String)>,
}

fn key_of(spec: &ConnectionSpec) -> Key {
    Key {
        driver: spec.driver.as_str().to_owned(),
        target: spec.target.clone(),
        ssh: spec
            .ssh
            .as_ref()
            .map(|s| (s.host.clone(), s.port, s.user.clone())),
    }
}

/// Folds a group path deeper than [`MAX_GROUP_DEPTH`] into its last allowed level.
fn cap_group(group: &[String], notes: &mut Vec<String>) -> Vec<String> {
    let mut path: Vec<String> = group
        .iter()
        .map(|g| g.trim().to_owned())
        .filter(|g| !g.is_empty())
        .collect();
    if path.len() > MAX_GROUP_DEPTH {
        let tail = path.split_off(MAX_GROUP_DEPTH - 1).join(" / ");
        path.push(tail);
        notes.push(format!(
            "group path is deeper than {MAX_GROUP_DEPTH} levels; the deeper levels were folded into the last one"
        ));
    }
    path
}

fn unique_name(name: &str, taken: &HashSet<String>) -> String {
    let mut n = 2_u64;
    loop {
        let candidate = format!("{name} ({n})");
        if !taken.contains(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// Plans the import of `report` into `store`.
///
/// Per entry, in this order: a skipped entry stays skipped; a driver missing from
/// `opts.available_drivers` is [`SkipReason::DriverUnavailable`]; the same server as a saved
/// connection is [`SkipReason::Duplicate`], as an earlier entry [`SkipReason::DuplicateInFile`];
/// a name already used in the store or the plan is renamed to `name (2)`, `name (3)`, ... and the
/// original is kept in `renamed_from`. Group paths are capped at [`MAX_GROUP_DEPTH`].
///
/// Pure: nothing is changed.
#[must_use]
pub fn plan(report: &SourceReport, store: &ConnectionStore, opts: &ImportOptions) -> ImportPlan {
    let mut taken: HashSet<String> = store
        .connections()
        .iter()
        .map(|c| c.spec().name.trim().to_owned())
        .collect();
    let existing: Vec<(Key, _)> = store
        .connections()
        .iter()
        .map(|c| (key_of(c.spec()), c.id().clone()))
        .collect();
    let mut seen: Vec<(Key, usize)> = Vec::new();

    let items = report
        .entries
        .iter()
        .map(|entry| plan_entry(entry, opts, &existing, &mut seen, &mut taken))
        .collect();
    ImportPlan {
        source: report.source,
        items,
        notices: report.notices.clone(),
    }
}

fn plan_entry(
    entry: &SourceEntry,
    opts: &ImportOptions,
    existing: &[(Key, ConnectionId)],
    seen: &mut Vec<(Key, usize)>,
    taken: &mut HashSet<String>,
) -> PlanItem {
    let mut notes = entry.notes.clone();
    let group = cap_group(&entry.group, &mut notes);
    let action = match &entry.outcome {
        EntryOutcome::Skipped(reason) => PlanAction::Skip(reason.clone()),
        EntryOutcome::Mapped(spec) => decide(entry.index, spec, opts, existing, seen, taken),
    };
    PlanItem {
        index: entry.index,
        foreign_name: entry.name.clone(),
        group,
        action,
        unsupported: entry.unsupported.clone(),
        notes,
        secrets: entry.secrets.clone(),
    }
}

fn decide(
    index: usize,
    spec: &ConnectionSpec,
    opts: &ImportOptions,
    existing: &[(Key, ConnectionId)],
    seen: &mut Vec<(Key, usize)>,
    taken: &mut HashSet<String>,
) -> PlanAction {
    let name = spec.name.trim();
    if name.is_empty() {
        return PlanAction::Skip(SkipReason::Invalid("the name is empty".to_owned()));
    }
    if !opts.available_drivers.contains(&spec.driver) {
        return PlanAction::Skip(SkipReason::DriverUnavailable {
            driver: spec.driver.clone(),
        });
    }
    let key = key_of(spec);
    if let Some((_, id)) = existing.iter().find(|(k, _)| *k == key) {
        return PlanAction::Skip(SkipReason::Duplicate {
            existing: id.clone(),
        });
    }
    if let Some((_, first)) = seen.iter().find(|(k, _)| *k == key) {
        return PlanAction::Skip(SkipReason::DuplicateInFile {
            first_index: *first,
        });
    }
    let (final_name, renamed_from) = if taken.contains(name) {
        (unique_name(name, taken), Some(name.to_owned()))
    } else {
        (name.to_owned(), None)
    };
    taken.insert(final_name.clone());
    let mut spec = spec.clone();
    spec.name = final_name;
    seen.push((key, index));
    PlanAction::Add { spec, renamed_from }
}
