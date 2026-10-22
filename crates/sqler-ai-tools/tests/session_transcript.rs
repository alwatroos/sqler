// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The transcript cap: old entries go at a turn boundary and stable numbers stay.
#![allow(clippy::unwrap_used)]

mod common;
mod session_common;

use common::SqliteSource;
use session_common::{provider, session, text};
use sqler_ai_tools::session::{
    MAX_TRANSCRIPT_ENTRIES, SessionLimits, TranscriptEntry, TurnOutcome,
};

#[tokio::test(flavor = "current_thread")]
async fn six_hundred_turns_stay_capped_and_keep_their_numbers() {
    let source = SqliteSource::new("transcript-cap", None).await;
    let p = provider();
    let turns = 600;
    for i in 0..turns {
        p.push_script(text(&format!("answer {i}")));
    }
    let mut s = session(&source, p, None, SessionLimits::default());
    for i in 0..turns {
        assert_eq!(
            s.send(&format!("question {i}")).await,
            TurnOutcome::Completed
        );
        let t = s.transcript();
        // One turn is a user entry and an answer: two entries.
        assert!(t.entries().len() <= MAX_TRANSCRIPT_ENTRIES + 2, "{i}");
        // Uncapped, question i is entry 2 * i and answer i is entry 2 * i + 1.
        let last = t.entries().len() - 1;
        assert_eq!(t.dropped() + last, 2 * i + 1);
        match &t.entries()[last] {
            TranscriptEntry::Assistant { text } => assert_eq!(text, &format!("answer {i}")),
            other => panic!("{other:?}"),
        }
    }
    let t = s.transcript();
    assert!(t.dropped() > 0);
    for (i, entry) in t.entries().iter().enumerate() {
        let n = t.dropped() + i;
        let expected = if n.is_multiple_of(2) {
            TranscriptEntry::User {
                text: format!("question {}", n / 2),
            }
        } else {
            TranscriptEntry::Assistant {
                text: format!("answer {}", n / 2),
            }
        };
        assert_eq!(entry, &expected, "number {n}");
    }
}
