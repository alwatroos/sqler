// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Independent G2 probes: run semantics per dialect and the cancel chord.

#![allow(clippy::unwrap_used)]

use sqler_config::{CommandId, Keymap, Platform};
use sqler_sql::Dialect;
use sqler_ui::query::{BufferSnapshot, QueryCommand, RunMode, action_for, plan};

fn sqls(p: &sqler_ui::query::Plan) -> Vec<String> {
    p.statements.iter().map(|s| s.sql.clone()).collect()
}

#[test]
fn probes_per_dialect() {
    for d in Dialect::ALL {
        for mode in [RunMode::Statement, RunMode::Selection] {
            let t = "update t\n\nset x=1";
            for pos in [0, 3, 8, 9, 10, 12, t.len()] {
                let p = plan(Some(d), &BufferSnapshot::at_cursor(t, pos), mode).unwrap();
                assert_eq!(sqls(&p), [t], "{d:?} {mode:?} {pos}");
            }
            let t = "select 1;\nselect 2 from u;";
            let p = plan(Some(d), &BufferSnapshot::at_cursor(t, 2), mode).unwrap();
            assert_eq!(sqls(&p), ["select 1"], "{d:?}");
            let p = plan(Some(d), &BufferSnapshot::at_cursor(t, 14), mode).unwrap();
            assert_eq!(sqls(&p), ["select 2 from u"], "{d:?}");
        }
        let t = "a1\nb2\nc3\nd4\ne5";
        let r = t.find("b2").unwrap()..t.find("\nd4").unwrap();
        for mode in [RunMode::Statement, RunMode::Selection] {
            let p = plan(Some(d), &BufferSnapshot::with_selection(t, r.clone()), mode).unwrap();
            assert_eq!(sqls(&p), ["b2\nc3"], "{d:?}");
        }
        let t = "x\nselect 1\n\nfrom t\ny";
        let r = 2..t.find("\ny").unwrap();
        for mode in [RunMode::Statement, RunMode::Selection] {
            let p = plan(Some(d), &BufferSnapshot::with_selection(t, r.clone()), mode).unwrap();
            assert_eq!(sqls(&p), ["select 1\n\nfrom t"], "{d:?}");
        }
    }
}

#[test]
fn cancel_chord_maps_to_cancel() {
    for (p, chord) in [(Platform::MacOs, "cmd+."), (Platform::Other, "ctrl+.")] {
        let km = Keymap::defaults(p);
        let id = CommandId::new("query:cancel").unwrap();
        let ch: Vec<String> = km.chords_for(&id).iter().map(|c| c.to_string()).collect();
        assert!(ch.iter().any(|c| c == chord), "{p:?} {ch:?}");
        let ui_id = sqler_ui::command::CommandId::new("query:cancel").unwrap();
        assert_eq!(action_for(&ui_id), Some(QueryCommand::Cancel));
        assert!(km.conflicts().is_empty(), "{p:?}");
    }
}
