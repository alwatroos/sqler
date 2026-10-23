// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The import core: plan rules, apply into a store and the secret store, secret hygiene.

#![allow(
    clippy::unwrap_used,
    clippy::assert_is_empty,
    clippy::many_single_char_names,
    clippy::too_many_lines,
    clippy::format_push_string
)]

use sqler_config::{
    ConfigDir, ConnectionSpec, ConnectionStore, MemoryStore, ProfileSecretKind, Secret,
    SecretError, SecretKind, SecretRef, SecretStore, SshAuth, SshProfile,
};
use sqler_core::DriverId;
use sqler_import::{
    EntryOutcome, ImportOptions, ImportPlan, MAX_GROUP_DEPTH, PgAdmin, PlanAction, PlanItem,
    ProvidedSecret, SkipReason, Source, SourceEntry, SourceReport, apply, plan,
};

const FAKE: &str = "fake-pw-123";

fn driver(id: &str) -> DriverId {
    DriverId::new(id).unwrap()
}

fn opts() -> ImportOptions {
    ImportOptions {
        available_drivers: vec![driver("postgres")],
    }
}

fn entry(index: usize, name: &str, driver_id: &str, target: &str) -> SourceEntry {
    SourceEntry {
        index,
        name: name.to_owned(),
        group: Vec::new(),
        outcome: EntryOutcome::Mapped(ConnectionSpec::new(name, driver(driver_id), target)),
        unsupported: Vec::new(),
        notes: Vec::new(),
        secrets: Vec::new(),
    }
}

fn report(entries: Vec<SourceEntry>) -> SourceReport {
    SourceReport {
        source: "pgadmin",
        entries,
        notices: vec!["a notice".to_owned()],
    }
}

fn action(plan: &ImportPlan, i: usize) -> &PlanAction {
    &plan.items[i].action
}

fn added_name(plan: &ImportPlan, i: usize) -> String {
    match action(plan, i) {
        PlanAction::Add { spec, .. } => spec.name.clone(),
        PlanAction::Skip(r) => panic!("skipped: {r:?}"),
    }
}

#[test]
fn plan_passes_notices_and_skips_through() {
    let mut skipped = entry(0, "s", "postgres", "postgres://h:1/d");
    skipped.outcome = EntryOutcome::Skipped(SkipReason::NotInThisRelease { engine: "DuckDB" });
    let p = plan(&report(vec![skipped]), &ConnectionStore::new(), &opts());
    assert_eq!(p.notices, ["a notice"]);
    assert_eq!(p.source, "pgadmin");
    let PlanAction::Skip(reason) = action(&p, 0) else {
        panic!("expected skip")
    };
    assert_eq!(reason.message(), "DuckDB is not in SQLer 1.0");
}

#[test]
fn unavailable_driver_is_skipped_by_the_callers_list() {
    let r = report(vec![
        entry(0, "a", "mssql", "mssql://h:1433/d"),
        entry(1, "b", "postgres", "postgres://h:1/d"),
    ]);
    let p = plan(&r, &ConnectionStore::new(), &opts());
    let PlanAction::Skip(reason) = action(&p, 0) else {
        panic!("expected skip")
    };
    assert_eq!(
        *reason,
        SkipReason::DriverUnavailable {
            driver: driver("mssql")
        }
    );
    assert_eq!(reason.message(), "not available in this build");
    assert!(matches!(action(&p, 1), PlanAction::Add { .. }));
    // An empty list makes everything unavailable: nothing is hard-coded.
    let none = plan(&r, &ConnectionStore::new(), &ImportOptions::default());
    assert!(
        none.items
            .iter()
            .all(|i| matches!(i.action, PlanAction::Skip(_)))
    );
}

#[test]
fn duplicates_against_the_store_and_the_file() {
    let mut store = ConnectionStore::new();
    let existing = store
        .add_connection(
            ConnectionSpec::new("Saved", driver("postgres"), "postgres://h:1/d"),
            None,
        )
        .unwrap();
    let r = report(vec![
        entry(0, "same as saved", "postgres", "postgres://h:1/d"),
        entry(1, "new", "postgres", "postgres://h:2/d"),
        entry(2, "again", "postgres", "postgres://h:2/d"),
    ]);
    let p = plan(&r, &store, &opts());
    assert!(matches!(
        action(&p, 0),
        PlanAction::Skip(SkipReason::Duplicate { existing: e }) if *e == existing
    ));
    assert!(matches!(action(&p, 1), PlanAction::Add { .. }));
    assert!(matches!(
        action(&p, 2),
        PlanAction::Skip(SkipReason::DuplicateInFile { first_index: 1 })
    ));
}

#[test]
fn ssh_tunnel_is_part_of_the_duplicate_key() {
    let mut store = ConnectionStore::new();
    store
        .add_connection(
            ConnectionSpec::new("Saved", driver("postgres"), "postgres://h:1/d"),
            None,
        )
        .unwrap();
    let mut tunnelled = entry(0, "tunnelled", "postgres", "postgres://h:1/d");
    if let EntryOutcome::Mapped(spec) = &mut tunnelled.outcome {
        spec.ssh = Some(SshProfile::new("bastion.example.test", "t", SshAuth::Agent));
    }
    let p = plan(&report(vec![tunnelled]), &store, &opts());
    assert!(matches!(action(&p, 0), PlanAction::Add { .. }));
}

#[test]
fn name_collisions_get_a_numbered_suffix() {
    let mut store = ConnectionStore::new();
    for (name, target) in [
        ("Prod", "postgres://x:1/d"),
        ("Prod (2)", "postgres://x:2/d"),
    ] {
        store
            .add_connection(ConnectionSpec::new(name, driver("postgres"), target), None)
            .unwrap();
    }
    let r = report(vec![
        entry(0, "Prod", "postgres", "postgres://a:1/d"),
        entry(1, " Prod ", "postgres", "postgres://b:1/d"),
        entry(2, "Fresh", "postgres", "postgres://c:1/d"),
        entry(3, "Fresh", "postgres", "postgres://d:1/d"),
    ]);
    let p = plan(&r, &store, &opts());
    assert_eq!(added_name(&p, 0), "Prod (3)");
    assert_eq!(added_name(&p, 1), "Prod (4)");
    assert_eq!(added_name(&p, 2), "Fresh");
    assert_eq!(added_name(&p, 3), "Fresh (2)");
    let PlanAction::Add { renamed_from, .. } = action(&p, 0) else {
        panic!()
    };
    assert_eq!(renamed_from.as_deref(), Some("Prod"));
    let PlanAction::Add { renamed_from, .. } = action(&p, 2) else {
        panic!()
    };
    assert!(renamed_from.is_none());
}

#[test]
fn empty_name_is_invalid() {
    let p = plan(
        &report(vec![entry(0, "  ", "postgres", "postgres://h:1/d")]),
        &ConnectionStore::new(),
        &opts(),
    );
    assert!(matches!(
        action(&p, 0),
        PlanAction::Skip(SkipReason::Invalid(_))
    ));
}

#[test]
fn deep_group_paths_are_folded() {
    let mut e = entry(0, "deep", "postgres", "postgres://h:1/d");
    e.group = (1..=MAX_GROUP_DEPTH + 3).map(|i| format!("g{i}")).collect();
    let p = plan(&report(vec![e]), &ConnectionStore::new(), &opts());
    let g = &p.items[0].group;
    assert_eq!(g.len(), MAX_GROUP_DEPTH);
    assert_eq!(g[MAX_GROUP_DEPTH - 2], format!("g{}", MAX_GROUP_DEPTH - 1));
    assert_eq!(g[MAX_GROUP_DEPTH - 1], "g8 / g9 / g10 / g11");
    assert!(p.items[0].notes.iter().any(|n| n.contains("folded")));
}

// ---- apply ----------------------------------------------------------------------------------

/// A secret store whose writes always fail.
struct FailingStore;

impl SecretStore for FailingStore {
    fn set(&self, r: &SecretRef, _secret: &Secret) -> Result<(), SecretError> {
        Err(SecretError::Unavailable {
            reference: r.clone(),
            reason: "locked".to_owned(),
        })
    }
    fn get(&self, _r: &SecretRef) -> Result<Option<Secret>, SecretError> {
        Ok(None)
    }
    fn delete(&self, _r: &SecretRef) -> Result<bool, SecretError> {
        Ok(false)
    }
}

fn pw(index: usize, kind: ProfileSecretKind) -> ProvidedSecret {
    ProvidedSecret {
        index,
        kind,
        secret: Secret::new(FAKE),
    }
}

fn full_report() -> SourceReport {
    let bytes = std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("pgadmin")
            .join("full.json"),
    )
    .unwrap();
    PgAdmin.parse(&bytes).unwrap()
}

fn store_with_existing() -> ConnectionStore {
    let mut store = ConnectionStore::new();
    let g = store.add_group("Shop", None).unwrap();
    // Same name as import entry 0, other target.
    store
        .add_connection(
            ConnectionSpec::new("Orders primary", driver("postgres"), "postgres://old:1/d")
                .with_password_in_keychain(true),
            Some(&g),
        )
        .unwrap();
    // Same target as import entry 1.
    store
        .add_connection(
            ConnectionSpec::new(
                "Existing twin",
                driver("postgres"),
                "postgres://staging.example.test:5432/postgres?user=bob",
            ),
            None,
        )
        .unwrap();
    store
}

#[test]
fn plan_then_apply_adds_only_new_things() {
    let mut store = store_with_existing();
    let before = store.clone();
    let p = plan(&full_report(), &store, &opts());

    // Entry 0 is renamed, entry 1 is a duplicate.
    let PlanAction::Add { spec, renamed_from } = action(&p, 0) else {
        panic!()
    };
    assert_eq!(spec.name, "Orders primary (2)");
    assert_eq!(renamed_from.as_deref(), Some("Orders primary"));
    assert!(matches!(
        action(&p, 1),
        PlanAction::Skip(SkipReason::Duplicate { .. })
    ));

    let secrets = MemoryStore::new();
    let provided = vec![
        pw(0, ProfileSecretKind::DbPassword),
        pw(2, ProfileSecretKind::SshPassphrase),
        pw(1, ProfileSecretKind::DbPassword), // skipped item: dropped
        pw(99, ProfileSecretKind::DbPassword), // unknown item: dropped
        pw(0, ProfileSecretKind::SshPassword), // not listed for the item: dropped
    ];
    let report = apply(&p, &mut store, &secrets, provided);
    assert!(report.failed.is_empty(), "{:?}", report.failed);

    // Existing profiles and groups are untouched.
    for old in before.connections() {
        assert_eq!(store.connection(old.id()), Some(old));
    }
    for old in before.groups() {
        assert_eq!(store.group(old.id()), Some(old));
    }
    assert_eq!(
        store.groups().len(),
        before.groups().len(),
        "Shop is reused"
    );

    // The new ones: user in the query, no user info, in the reused group.
    let (_, id0) = report.added.iter().find(|(i, _)| *i == 0).unwrap();
    let new0 = store.connection(id0).unwrap();
    assert_eq!(new0.spec().name, "Orders primary (2)");
    assert!(new0.spec().target.contains("?user=alice"));
    assert!(!new0.spec().target.contains('@'));
    assert_eq!(
        new0.group(),
        before.groups().first().map(sqler_config::Group::id)
    );
    assert!(new0.spec().password_in_keychain);
    let r = SecretRef::new(id0.as_str(), SecretKind::DatabasePassword).unwrap();
    assert_eq!(r.to_string(), format!("{id0}:db-password"));
    assert_eq!(secrets.get(&r).unwrap().unwrap().expose(), FAKE);

    // Entry 2 (ssh key): passphrase flag set and stored; no db password given.
    let (_, id2) = report.added.iter().find(|(i, _)| *i == 2).unwrap();
    let new2 = store.connection(id2).unwrap();
    assert!(!new2.spec().password_in_keychain);
    #[cfg(unix)]
    {
        assert!(matches!(
            new2.spec().ssh.as_ref().unwrap().auth,
            SshAuth::KeyFile {
                passphrase_in_keychain: true,
                ..
            }
        ));
        assert!(
            report
                .secrets_stored
                .contains(&(2, ProfileSecretKind::SshPassphrase))
        );
    }
    assert!(
        report
            .secrets_skipped
            .contains(&(2, ProfileSecretKind::DbPassword))
    );
    assert!(
        report
            .secrets_stored
            .contains(&(0, ProfileSecretKind::DbPassword))
    );

    // Nothing for the skipped, unknown or unlisted requests.
    assert!(!report.added.iter().any(|(i, _)| *i == 1));
    let r_ssh = SecretRef::new(id0.as_str(), SecretKind::SshPassword).unwrap();
    assert!(secrets.get(&r_ssh).unwrap().is_none());
    // Exactly the stored secrets are in the store (the key-file passphrase only where paths are absolute).
    assert_eq!(report.secrets_stored.len(), if cfg!(unix) { 2 } else { 1 });

    // Entry 3: ssh password listed but not provided: flag stays absent, reported as skipped.
    assert!(
        report
            .secrets_skipped
            .contains(&(3, ProfileSecretKind::SshPassword))
    );
}

#[test]
fn a_failing_secret_store_clears_the_flag() {
    let mut store = ConnectionStore::new();
    let mut e = entry(0, "Only", "postgres", "postgres://h:1/d?user=alice");
    e.secrets = vec![ProfileSecretKind::DbPassword];
    let p = plan(&report(vec![e]), &store, &opts());
    let report = apply(
        &p,
        &mut store,
        &FailingStore,
        vec![pw(0, ProfileSecretKind::DbPassword)],
    );
    assert!(report.failed.is_empty());
    assert!(report.secrets_stored.is_empty());
    assert_eq!(report.secrets_skipped, [(0, ProfileSecretKind::DbPassword)]);
    let (_, id) = &report.added[0];
    assert!(!store.connection(id).unwrap().spec().password_in_keychain);
    assert!(!format!("{report:?}").contains(FAKE));
}

#[test]
fn no_provided_secret_leaves_the_flag_false() {
    let mut store = ConnectionStore::new();
    let mut e = entry(0, "Only", "postgres", "postgres://h:1/d?user=alice");
    e.secrets = vec![ProfileSecretKind::DbPassword];
    let p = plan(&report(vec![e]), &store, &opts());
    let secrets = MemoryStore::new();
    // An empty value counts as not provided.
    let empty = ProvidedSecret {
        index: 0,
        kind: ProfileSecretKind::DbPassword,
        secret: Secret::new(""),
    };
    let report = apply(&p, &mut store, &secrets, vec![empty]);
    let (_, id) = &report.added[0];
    assert!(!store.connection(id).unwrap().spec().password_in_keychain);
    assert_eq!(report.secrets_skipped, [(0, ProfileSecretKind::DbPassword)]);
    assert_eq!(format!("{secrets:?}"), "MemoryStore { len: 0 }");
}

#[test]
fn a_plan_the_caller_trimmed_applies_only_what_is_left() {
    let mut store = ConnectionStore::new();
    let r = report(vec![
        entry(0, "A", "postgres", "postgres://a:1/d"),
        entry(1, "B", "postgres", "postgres://b:1/d"),
    ]);
    let mut p = plan(&r, &store, &opts());
    p.items.retain(|i: &PlanItem| i.index == 1);
    let report = apply(&p, &mut store, &MemoryStore::new(), Vec::new());
    assert_eq!(report.added.len(), 1);
    assert_eq!(store.connections().len(), 1);
}

#[test]
fn a_stale_plan_reports_a_failure_and_changes_nothing() {
    let mut store = ConnectionStore::new();
    let r = report(vec![entry(0, "A", "postgres", "postgres://a:1/d")]);
    let p = plan(&r, &store, &opts());
    // The store gained the same name after the plan was made.
    store
        .add_connection(
            ConnectionSpec::new("A", driver("postgres"), "postgres://z:1/d"),
            None,
        )
        .unwrap();
    let before = store.clone();
    let report = apply(&p, &mut store, &MemoryStore::new(), Vec::new());
    assert!(report.added.is_empty());
    assert_eq!(report.failed.len(), 1);
    assert_eq!(store, before);
}

#[test]
fn groups_are_created_by_path_and_reused() {
    let mut store = ConnectionStore::new();
    let mut a = entry(0, "A", "postgres", "postgres://a:1/d");
    a.group = vec!["One".to_owned(), "Two".to_owned()];
    let mut b = entry(1, "B", "postgres", "postgres://b:1/d");
    b.group = vec!["One".to_owned(), "Two".to_owned()];
    let p = plan(&report(vec![a, b]), &store, &opts());
    let report = apply(&p, &mut store, &MemoryStore::new(), Vec::new());
    assert_eq!(report.added.len(), 2);
    assert_eq!(store.groups().len(), 2);
    let two = store.groups().iter().find(|g| g.name() == "Two").unwrap();
    assert_eq!(store.child_connections(Some(two.id())).count(), 2);
}

#[test]
fn a_secret_never_reaches_debug_output_errors_or_the_config_file() {
    let mut store = store_with_existing();
    let p = plan(&full_report(), &store, &opts());
    let secrets = MemoryStore::new();
    let provided = vec![
        pw(0, ProfileSecretKind::DbPassword),
        pw(3, ProfileSecretKind::SshPassword),
    ];
    let provided_debug = format!("{provided:?}");
    let report = apply(&p, &mut store, &secrets, provided);

    let dir = std::env::temp_dir().join(format!("sqler-import-core-{}", std::process::id()));
    let config = ConfigDir::new(&dir);
    config.save_connection_store(&store).unwrap();
    let toml = std::fs::read_to_string(config.connections_path()).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();

    let mut texts = vec![
        format!("{p:?}"),
        format!("{report:?}"),
        format!("{store:?}"),
        format!("{secrets:?}"),
        provided_debug,
        toml,
    ];
    let reasons: Vec<String> = p
        .items
        .iter()
        .filter_map(|i| match &i.action {
            PlanAction::Skip(r) => Some(r.message()),
            PlanAction::Add { .. } => None,
        })
        .collect();
    texts.extend(reasons);
    texts.extend(report.failed.iter().map(|(_, t)| t.clone()));
    for t in texts {
        assert!(!t.contains(FAKE), "secret leaked");
    }
}
