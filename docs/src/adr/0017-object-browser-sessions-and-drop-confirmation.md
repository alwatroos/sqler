# 0017 Object browser: browser session per connection, short-lived action sessions, DROP confirmation

- Status: accepted
- Date: 2026-10-20

## Context and problem statement

The sidebar must show the objects of an open connection, load them lazily, and offer actions on them: open data, DDL, properties, copy name, refresh and DROP. Three things need a decision: which sessions run this work, how DROP is made safe, and how the tree logic stays testable without a display. The catalog worker ([ADR-0014](0014-catalog-cache-and-background-refresh.md)) serialises introspection on one session, DDL text comes from `sqler-sql` ([ADR-0015](0015-ddl-generation-dialects-and-quoting.md)), and all tokio work goes through `SessionBridge` ([ADR-0007](0007-desktop-binary-and-ui-tokio-bridge.md), [ADR-0016](0016-connect-pipeline.md)).

## Decision drivers

- The UI thread never blocks and never calls `tokio::spawn`.
- A session is never shared across an `.await` behind a lock (AGENTS.md §4).
- A DROP must not run by accident, and on `prod` not by a single stray click.
- Tree logic must be unit-tested headless; views are build-only on CI.
- `sqler-ui` must not import drivers.

## Considered options

1. One shared utility session per connection for all actions, behind a lock or a check-out protocol.
2. One browser session per connection owned by a `CatalogWorker`, plus a short-lived session per action that needs SQL or `Introspect::definition`.
3. A new session for everything, including tree loads.

## Decision outcome

Chosen: option 2.

### Session model

After the first successful `connection:open`, the workspace connects once more (`Purpose::Browse`) and hands that session to a `CatalogWorker`. The tree loads only through `Catalog::children` and `Catalog::table`, so it inherits the cache, the deduplication and the refresh with progress and cancel. Open data, CREATE text and DROP each open a short-lived session with `Connector` and close it when the job ends. The browser ends when the connection is edited or deleted, and on quit.

### The tree is pure

`ObjectTree` never calls the catalog. `expand` and the `*_loaded` methods return the `CacheKey`s to load; the workspace turns them into effects and the view resolves them in `cx.spawn`. This is the effect pattern of the workspace, so the tree is tested with no runtime. The sidebar renders a virtualized list over cached rows, so 5,000 rows are never built at once.

### DROP confirmation

The statement comes from `drop_script` once, when `DropFlow::start` is called. It is shown verbatim in the dialog. `DropFlow::confirm` is the only function that returns the text to run: the first call returns it, or on a `prod` connection moves to a second step with its own button ("Drop on production") and returns it on the next call. The user does not type the name. There is no `CASCADE`. The tree reloads the parent after a successful DROP; a failure is a notice and the tree is untouched. The second step ignores a confirmation within `PROD_GUARD` (800 ms) of appearing, a click with `click_count > 1` and held (auto-repeat) keys, and its button has its own element id and place. The time is passed in (`confirm_at`) so the guard is unit-tested headless; the click and key paths need a visual check.

### New public API

`sqler_ui::browser` is a new public module: `ObjectTree`, `TreeNodeId`, `TreeNodeKind`, `LoadState`, `TreeRow`, `MenuEntry`, `menu_for`, `BrowserAction`, `COMMANDS`, `register_commands`, `action_for`, `DropFlow`, `DropStep`, `DropDialog`, `ObjectTab`, `ObjectTabContent`, `Property`, `BrowserError` and the jobs (`generate_script`, `load_properties`, `open_data`, `run_drop`). The 19 command IDs `object:*` are stable. No new external dependency; `sqler-ui` gains the internal `sqler-catalog` and `sqler-sql`.

### Consequences

- Positive: no lock across `.await`; tree, menus and the DROP flow are tested headless; a refresh or DROP cannot stall the browser session.
- Negative: each action opens a session, which is a connect (and an SSH tunnel) per action. Accepted for M2; a pool or a reused utility session can replace it later without changing the tree.
- Negative: a connection costs two sessions (query tab and browser), plus the short-lived ones.
- Negative: `gpui-component` 0.8.0 cannot open a context menu from code, so the keyboard path (Shift+F10, Menu) opens the command palette filtered to `Object: ` instead of a menu at the row.
- Negative: `DROP FUNCTION` for an overloaded `PostgreSQL` function fails with the server's error ([ADR-0015](0015-ddl-generation-dialects-and-quoting.md)).
- Negative (known limit): while the DROP runs, the "Dropping..." step occludes the window until the server returns; a DROP that waits for a lock keeps the app modal. A cancel is left for a later item.
- Not checked by CI (maintainer, on a display): the context menu, the DROP dialog and the prod second step, the DDL and properties tabs, scrolling 5,000 nodes, keyboard navigation and the clipboard.
