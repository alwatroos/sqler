// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! A session seeded with a restored chat sends its text as history and continues the numbers.
#![allow(clippy::unwrap_used)]

mod common;
mod session_common;

use common::{SqliteSource, limits};
use session_common::{TestProvider, events, text};
use sqler_ai_api::Role;
use sqler_ai_tools::ToolSet;
use sqler_ai_tools::session::{
    ExplorationSession, MemoryEvents, PriorChat, SessionConfig, TranscriptEntry, TurnOutcome,
    restored_entry,
};
use sqler_config::StoredEntry;

fn prior() -> PriorChat {
    let stored = [
        StoredEntry::User {
            text: "how many orders?".into(),
        },
        StoredEntry::Sql {
            call_id: "c1".into(),
            connection: "db".into(),
            display_sql: "select count(*) from orders".into(),
            class: "read".into(),
            approved: false,
            rows: 1,
            truncated: None,
            failed: false,
        },
        StoredEntry::Assistant {
            text: "There are 3.".into(),
        },
    ];
    PriorChat::new(stored.into_iter().filter_map(restored_entry).collect(), 10)
}

#[tokio::test(flavor = "current_thread")]
async fn the_first_request_carries_the_prior_text_and_the_numbers_continue() {
    let source = SqliteSource::new("restore", None).await;
    let provider = TestProvider::new(vec![events(text("Now 4."))]);
    let tools = ToolSet::new(source.clone(), limits());
    let config = SessionConfig::new("test-model");
    let events_sink = MemoryEvents::new();
    let mut session =
        ExplorationSession::new(provider.clone(), tools, source.info.clone(), None, config)
            .unwrap()
            .with_events(events_sink.clone())
            .with_prior(prior());

    // Restoring emits nothing and shows the entries at their stable numbers.
    assert_eq!(events_sink.events().len(), 0);
    assert_eq!(session.transcript().dropped(), 10);
    assert_eq!(session.transcript().entries().len(), 3);

    assert_eq!(session.send("and now?").await, TurnOutcome::Completed);

    let requests = provider.requests();
    assert_eq!(requests.len(), 1);
    let messages: Vec<_> = requests[0]
        .messages
        .iter()
        .map(|m| (m.role(), m.text()))
        .collect();
    assert_eq!(messages.len(), 3, "{messages:?}");
    assert_eq!(messages[0].0, Role::User);
    assert!(messages[0].1.starts_with("Restored chat:"));
    assert!(messages[0].1.ends_with("how many orders?"));
    assert_eq!(messages[1].0, Role::Assistant);
    assert_eq!(
        messages[1].1,
        "SQL run earlier:\nselect count(*) from orders\n\nThere are 3."
    );
    assert_eq!(messages[2], (Role::User, "and now?".to_owned()));
    // No tool call is replayed.
    assert!(requests[0].messages.iter().all(|m| m.parts().len() == 1));

    let t = session.transcript();
    assert_eq!(t.dropped(), 10);
    assert_eq!(t.entries().len(), 5);
    assert_eq!(
        t.entries()[3],
        TranscriptEntry::User {
            text: "and now?".into()
        }
    );
    assert_eq!(
        t.entries()[4],
        TranscriptEntry::Assistant {
            text: "Now 4.".into()
        }
    );
}
