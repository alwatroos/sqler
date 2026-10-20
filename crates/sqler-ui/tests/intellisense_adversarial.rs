// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Adversarial headless exercises of the intellisense core: unicode, huge catalogs, on-demand columns,
//! refresh generations, unterminated tokens, cursor at the buffer ends and empty catalogs.

#![allow(
    clippy::unwrap_used,
    clippy::many_single_char_names,
    clippy::assert_is_empty,
    clippy::assertions_on_result_states,
    clippy::bool_to_int_with_if,
    clippy::format_collect,
    clippy::manual_assert,
    clippy::bool_assert_comparison,
    clippy::assertions_on_constants
)]

use sqler_catalog::{Catalog, RefreshScope};
use sqler_completion::diagnose;
use sqler_sql::Dialect;
use sqler_ui::editor::intellisense::synthetic::{synthetic_catalog, synthetic_catalog_with};
use sqler_ui::editor::intellisense::{
    CatalogSchema, bootstrap_keys, complete_popup, flatten_snippet, highlight_ranges,
    hover_markdown, is_completion_trigger, load, prefetch_keys,
};

const ALL: [Dialect; 3] = [Dialect::Postgres, Dialect::MySql, Dialect::Sqlite];

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

fn ready(rt: &tokio::runtime::Runtime, tables: usize) -> Catalog {
    let (c, w) = synthetic_catalog(tables, 4);
    rt.spawn(w.run());
    rt.block_on(load(&c, bootstrap_keys()));
    c
}

fn schema(c: &Catalog, d: Dialect) -> CatalogSchema {
    CatalogSchema::new(c.snapshot(), d)
}

fn texts() -> Vec<String> {
    let mut v: Vec<String> = [
        "",
        "S",
        "SELECT",
        "SELECT ",
        "SELECT 'abc",
        "SELECT \"abc",
        "SELECT `a",
        "SELECT 1 /* x",
        "SELECT -- c",
        "SELECT $$ x",
        "SELECT * FROM \u{4e2d}\u{6587}",
        "SELECT \u{1F600} FROM t_00001 \u{e9}",
        "SELECT \u{e9}. FROM t_00001 \u{e9}",
        "SELECT u. FROM t_00001 u WHERE 'é",
        "SELECT [",
        ".",
        "..",
        "a.b.c.d.e.",
        "\u{0}\u{0}",
        "SELECT * FROM t_00001 a JOIN t_00002 b ON a.",
        "SELECT \u{200d}\u{301}x",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    v.push("SELECT x FROM t_00001;\n".repeat(200));
    v
}

#[test]
fn every_offset_every_dialect_never_panics() {
    let rt = rt();
    let c = ready(&rt, 50);
    for d in ALL {
        let s = schema(&c, d);
        for t in texts() {
            let step = if t.len() > 500 { 997 } else { 1 };
            let mut i = 0;
            while i <= t.len() {
                if t.is_char_boundary(i) {
                    let p = complete_popup(&t, i, d, &s);
                    assert!(
                        p.replace.start <= p.replace.end && p.replace.end <= t.len(),
                        "{t:?}@{i}"
                    );
                    assert!(
                        t.is_char_boundary(p.replace.start) && t.is_char_boundary(p.replace.end),
                        "{t:?}@{i}"
                    );
                    if let Some((r, md)) = hover_markdown(&t, i, d, &s) {
                        assert!(
                            r.end <= t.len()
                                && t.is_char_boundary(r.start)
                                && t.is_char_boundary(r.end)
                        );
                        assert!(!md.is_empty());
                    }
                }
                i += step;
            }
            for dg in diagnose(&t, d, &s) {
                assert!(
                    dg.range.end <= t.len() && dg.range.start <= dg.range.end,
                    "{t:?}"
                );
                assert!(
                    t.is_char_boundary(dg.range.start) && t.is_char_boundary(dg.range.end),
                    "{t:?} {dg:?}"
                );
            }
            let _ = prefetch_keys(&t, d, &s, 64);
        }
    }
}

#[test]
fn offsets_beyond_the_buffer_or_inside_a_char_do_not_panic() {
    let rt = rt();
    let c = ready(&rt, 5);
    let s = schema(&c, Dialect::Postgres);
    let t = "SELECT \u{e9}\u{1F600} FROM t_00001";
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        for off in [t.len() + 1, t.len() + 100, usize::MAX, 8, 9, 10] {
            let _ = complete_popup(t, off, Dialect::Postgres, &s);
            let _ = hover_markdown(t, off, Dialect::Postgres, &s);
        }
    }));
    assert!(r.is_ok(), "panic on out-of-range or mid-char offset");
}

#[test]
fn empty_and_unloaded_catalogs() {
    let rt = rt();
    // 0 tables, loaded.
    let c = ready(&rt, 0);
    for d in ALL {
        let s = schema(&c, d);
        let p = complete_popup("SELECT * FROM ", 14, d, &s);
        assert!(p.items.iter().all(|i| !i.label.starts_with("t_")));
        let _ = diagnose("SELECT * FROM nope", d, &s);
    }
    // Never loaded at all.
    let (c, w) = synthetic_catalog(10, 2);
    rt.spawn(w.run());
    for d in ALL {
        let s = schema(&c, d);
        let _ = complete_popup("SELECT * FROM ", 14, d, &s);
        let _ = hover_markdown("SELECT * FROM t_00001", 18, d, &s);
        assert!(prefetch_keys("SELECT * FROM t_00001", d, &s, 64).is_empty());
    }
}

#[test]
fn huge_catalog_caps_the_popup_and_loads_columns_on_demand() {
    let rt = rt();
    let c = ready(&rt, 5000);
    let s = schema(&c, Dialect::Postgres);
    let t = "SELECT * FROM ";
    let p = complete_popup(t, t.len(), Dialect::Postgres, &s);
    assert!(
        p.items.len() <= 200 && !p.items.is_empty(),
        "{}",
        p.items.len()
    );
    let sorted: Vec<_> = p.items.iter().map(|i| i.sort_text.clone()).collect();
    let mut s2 = sorted.clone();
    s2.sort();
    assert_eq!(sorted, s2);

    let q = "SELECT t. FROM t_04999 t";
    let off = "SELECT t.".len();
    let a = complete_popup(q, off, Dialect::Postgres, &s);
    assert!(a.items.iter().all(|i| i.label != "c_1"));
    assert_eq!(a.loads.len(), 1);
    // Hover/diagnostics before loading must stay silent for unloaded columns.
    let d = diagnose("SELECT t.zzz FROM t_04999 t", Dialect::Postgres, &s);
    assert!(
        d.is_empty(),
        "no false positive while columns are unloaded: {d:?}"
    );
    rt.block_on(load(&c, a.loads));
    let s = schema(&c, Dialect::Postgres);
    let b = complete_popup(q, off, Dialect::Postgres, &s);
    assert!(b.items.iter().any(|i| i.label == "c_1"));
    assert!(b.loads.is_empty());
    let d = diagnose("SELECT t.zzz FROM t_04999 t", Dialect::Postgres, &s);
    assert_eq!(d.len(), 1, "{d:?}");
    let h = hover_markdown("SELECT c_1 FROM t_04999", 9, Dialect::Postgres, &s).expect("hover");
    assert!(h.1.contains("c_1"), "{}", h.1);
    // Prefetch is capped.
    let many: String = (0..300)
        .map(|i| format!("SELECT * FROM t_{i:05};"))
        .collect();
    let k = prefetch_keys(&many, Dialect::Postgres, &s, 64);
    assert_eq!(k.len(), 64);
    // Loading 64 tables completes.
    rt.block_on(load(&c, k));
}

#[test]
fn refresh_changes_generation_and_old_snapshot_stays_stable() {
    let rt = rt();
    let (c, w, h) = synthetic_catalog_with(5, 3);
    rt.spawn(w.run());
    rt.block_on(load(&c, bootstrap_keys()));
    let old = schema(&c, Dialect::Postgres);
    let g = c.snapshot().generation();
    h.set_tables(0);
    let mut hd = c.refresh(RefreshScope::All);
    rt.block_on(async { while hd.next_event().await.is_some() {} });
    assert_ne!(g, c.snapshot().generation());
    let t = "SELECT * FROM ";
    let has = |s: &CatalogSchema| {
        complete_popup(t, t.len(), Dialect::Postgres, s)
            .items
            .iter()
            .any(|i| i.label == "t_00001")
    };
    assert!(has(&old), "old snapshot unchanged");
    assert!(
        !has(&schema(&c, Dialect::Postgres)),
        "new snapshot reflects removal"
    );
    h.set_tables(8);
    let mut hd = c.refresh(RefreshScope::All);
    rt.block_on(async { while hd.next_event().await.is_some() {} });
    assert!(has(&schema(&c, Dialect::Postgres)));
}

#[test]
fn small_helpers_with_unicode() {
    assert!(is_completion_trigger("\u{e9}") && is_completion_trigger("."));
    assert!(
        !is_completion_trigger("")
            && !is_completion_trigger("ab")
            && !is_completion_trigger(" ")
            && !is_completion_trigger("\u{1F600}")
    );
    for r in highlight_ranges("\u{c9}", "caf\u{e9}\u{1F600}") {
        assert!(
            "caf\u{e9}\u{1F600}".is_char_boundary(r.start)
                && "caf\u{e9}\u{1F600}".is_char_boundary(r.end)
        );
    }
    assert_eq!(
        highlight_ranges("", "abc"),
        Vec::<std::ops::Range<usize>>::new()
    );
    assert!(highlight_ranges("zzz", "abc").is_empty());
    let _ = highlight_ranges("\"\"\"", "x");
    assert_eq!(flatten_snippet("a${1:b}$0"), "ab");
    let _ = flatten_snippet("${");
    let _ = flatten_snippet("\\");
    let _ = flatten_snippet("${1:\u{e9}\u{1F600}}$");
}
