# Object browser

Every connection that is open shows its objects under its row in the sidebar. The tree loads lazily: nothing is fetched until you expand a node. The tree has one extra session per connection, so browsing never waits for a query tab.

## The tree

```text
connection
  schema
    Tables (n)
      table
        Columns (n)
          column
        Triggers (n)
        Indexes (n)
    Views (n)
    ...
```

- Expanding a node shows a loading state on its row, then the children. A folder with nothing in it shows `(0)` and cannot be expanded.
- Only the kinds the driver supports appear. Tables, views and materialized views have the `Columns`, `Triggers` and `Indexes` folders.
- A failed load shows its error on the row. Expanding the row again retries. A connection that failed to start its browser shows the error on its root; expanding the root retries.
- The browser starts after the first successful `connection:open`. It ends when you edit or delete the connection, or quit.
- The sidebar is a virtual list, so a schema with 5,000 tables scrolls like a short one.

## Context menu

Right-click a row. Entries that do not apply are left out.

| Node | Entries |
| ---- | ------- |
| Connection, folder, `Columns` | Refresh |
| Table, view and other objects | Open Data, Generate CREATE, Generate SELECT, Generate INSERT, Generate DROP, Drop, Copy Name, Refresh, Properties |
| Column | Copy Name, Properties |

The script entries follow the object kind and the dialect, see [DDL and scripts](ddl.md): `INSERT` is for tables, `SELECT` for tables and views. A driver without a SQL dialect has no scripts, no Open Data and no Copy Name.

- **Open Data** opens a [results grid](results-grid.md) tab, titled `Results: <object>`, with `SELECT * ... LIMIT 1000`. It runs on its own short-lived session; closing the tab cancels it.
- **Generate CREATE, SELECT, INSERT, DROP** opens a read-only `DDL: <object>` tab with the script and a Copy button. Nothing runs against the server, except Generate CREATE for a view, function, index and the like: it opens a short-lived session to read the definition.
- **Properties** opens a read-only `Properties: <object>` tab with a two-column table of the fields SQLer knows (kind, path, owner, comment, columns and so on).
- **Copy Name** puts the fully quoted, qualified name on the clipboard.
- **Refresh** reloads the node and everything below it. The row shows progress while it runs and a cancel link; `Object: Cancel Refresh` does the same. Cancelling keeps the old tree.

## Drop

**Drop** never runs on the first click. A dialog shows the exact statement, for example `DROP TABLE "main"."users"`, with the buttons Cancel and Drop. SQLer adds no `CASCADE`: if other objects depend on it, the server's error is shown in a notice and nothing is lost. Escape cancels.

On a connection with the `prod` environment a second dialog follows ("Production connection ...") with the button "Drop on production". Only after it does the statement run. The tree reloads the parent afterwards. `DROP FUNCTION` on `PostgreSQL` fails for overloaded functions; the error is shown ([ADR-0015](../adr/0015-ddl-generation-dialects-and-quoting.md)).

A double click does the default action: Open Data for an object, otherwise expand or collapse.

## Keyboard

Click the sidebar, or move focus to it, and these work:

| Key | Command |
| --- | ------- |
| Up, Down | `object:select-previous`, `object:select-next` |
| Right, Left | `object:expand`, `object:collapse` |
| Enter | `object:open-data` (a notice on a node that has no data) |
| Shift+F10 or Menu | `object:show-menu` |

`gpui-component` 0.8.0 cannot open a context menu from code, so Shift+F10 and Menu open the [command palette](keybindings.md) filtered to `Object:`. It lists the commands below; the ones that do not apply to the selected node show a notice.

## Commands

All of them can be bound in `keybindings.toml`; none has a default shortcut.

| Command ID | Title |
| ---------- | ----- |
| `object:open-data` | Object: Open Data |
| `object:generate-create` | Object: Generate CREATE |
| `object:generate-select` | Object: Generate SELECT |
| `object:generate-insert` | Object: Generate INSERT |
| `object:generate-drop` | Object: Generate DROP |
| `object:drop` | Object: Drop |
| `object:copy-name` | Object: Copy Name |
| `object:refresh` | Object: Refresh |
| `object:cancel-refresh` | Object: Cancel Refresh |
| `object:properties` | Object: Properties |
| `object:expand`, `object:collapse`, `object:toggle` | Object: Expand, Collapse, Toggle |
| `object:select-next`, `object:select-previous` | Object: Select Next, Select Previous |
| `object:show-menu` | Object: Show Context Menu |
| `object:confirm-drop`, `object:cancel-drop` | Object: Confirm Drop, Cancel Drop |
| `object:copy-tab-text` | Object: Copy Tab Text |

## Limits

- Open Data, Generate CREATE (for objects other than tables) and Drop each open a short-lived session, so a connection behind an SSH tunnel opens a tunnel per action. Properties and the other scripts use only the catalog.
- While Drop runs, the dialog shows "Dropping..." and the window is blocked until the server answers. A DROP that waits for a lock on the server keeps the app modal until it returns; there is no cancel yet.
- On a production connection the second Drop button ignores a click or key press within 0.8 s of appearing, a double click, and held keys.
- Searching the tree is not implemented yet.
