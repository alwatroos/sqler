// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Driver fixture plugin (`driver-catalog-plugin` world): a guest driver over a fake in-memory database. The statement
//! text picks the behaviour, so the host tests (`crates/sqler-plugin-host/tests/guests/driver.rs`) can drive every
//! path of the adapter: streaming, affected rows, cancel, a trap in the second batch, malformed batches, and the
//! catalog.

// The wit-bindgen macro expands to `unsafe` FFI glue and undocumented items (AGENTS.md section 4 exception),
// so the generated code and the exports live in one module with the lints relaxed.
#[allow(unsafe_code, missing_docs, clippy::all, clippy::pedantic)]
mod bindings {
    wit_bindgen::generate!({
        path: "../../../crates/sqler-plugin-api/wit",
        world: "driver-catalog-plugin",
    });

    use std::cell::Cell;

    use self::exports::sqler::plugin::driver::{
        ConnectParams, Execution, Guest, GuestRowStream, GuestSession, RowStream, Session,
        SessionBorrow,
    };
    use self::exports::sqler::plugin::driver_catalog::{self, TableColumn};
    use self::exports::sqler::plugin::lifecycle;
    use self::sqler::plugin::log::{Level, write};
    use self::sqler::plugin::types::{
        Column, DataType, DbObject, ErrorCode, ObjectKind, ObjectPath, PluginError, Value,
    };

    struct Fixture;

    fn fail(message: impl Into<String>) -> PluginError {
        PluginError {
            code: ErrorCode::Failed,
            message: message.into(),
        }
    }

    impl lifecycle::Guest for Fixture {
        fn init() -> Result<(), lifecycle::PluginError> {
            write(Level::Info, "driver-fixture: init");
            Ok(())
        }
    }

    impl Guest for Fixture {
        type RowStream = Stream;
        type Session = Db;

        fn connect(driver: String, params: ConnectParams) -> Result<Session, PluginError> {
            if driver != "fixture-db" {
                return Err(fail(format!("unknown driver {driver}")));
            }
            // The fixture refuses a missing password: it proves the connect pipeline's hand-off.
            let Some(password) = params.password.clone() else {
                return Err(fail("credentials are required"));
            };
            if params.database == "leak" {
                // A guest that echoes the secret in its error: the host must not pass this text on.
                return Err(fail(format!("access denied, password was {password}")));
            }
            Ok(Session::new(Db { params, password }))
        }
    }

    /// An open connection.
    struct Db {
        params: ConnectParams,
        password: String,
    }

    fn col(name: &str, data_type: DataType) -> Column {
        Column {
            name: name.to_owned(),
            data_type,
            type_name: String::new(),
        }
    }

    impl GuestSession for Db {
        fn execute(&self, sql: String, params: Vec<Value>) -> Result<Execution, PluginError> {
            let (verb, rest) = sql.split_once(' ').unwrap_or((sql.as_str(), ""));
            let total: u32 = rest.trim().parse().unwrap_or(0);
            let stream = |mode: Mode, columns: Vec<Column>| {
                Ok(Execution::Rows(RowStream::new(Stream {
                    mode,
                    columns,
                    sent: Cell::new(0),
                    batches: Cell::new(0),
                })))
            };
            match verb {
                "rows" => stream(
                    Mode::Rows(total),
                    vec![col("n", DataType::Integer), col("s", DataType::Text)],
                ),
                "affected" => Ok(Execution::Affected(u64::from(total))),
                "echo" => stream(Mode::Echo(params), vec![col("p", DataType::Other)]),
                "whoami" => stream(
                    Mode::Fixed(vec![vec![
                        Value::Text(self.params.host.clone()),
                        Value::Text(self.params.user.clone()),
                        Value::Text(self.params.database.clone()),
                        Value::Text(self.password.clone()),
                        Value::Integer(i64::try_from(self.params.options.len()).unwrap_or(-1)),
                        Value::Integer(self.params.port.map_or(-1, i64::from)),
                    ]]),
                    vec![
                        col("host", DataType::Text),
                        col("user", DataType::Text),
                        col("database", DataType::Text),
                        col("password", DataType::Text),
                        col("options", DataType::Integer),
                        col("port", DataType::Integer),
                    ],
                ),
                "error" => Err(fail("no such table fixture")),
                "spin" => stream(Mode::Spin, vec![col("n", DataType::Integer)]),
                "trap-second" => stream(Mode::TrapSecond, vec![col("n", DataType::Integer)]),
                "wide" => stream(Mode::Wide, vec![col("n", DataType::Integer)]),
                "many" => stream(Mode::Many, vec![col("n", DataType::Integer)]),
                "bad-date" => stream(Mode::BadDate, vec![col("d", DataType::Date)]),
                _ => Err(fail(format!("unsupported statement {verb}"))),
            }
        }

        fn close(&self) {}
    }

    enum Mode {
        Rows(u32),
        Echo(Vec<Value>),
        Fixed(Vec<Vec<Value>>),
        Spin,
        TrapSecond,
        Wide,
        Many,
        BadDate,
    }

    /// A result being read.
    struct Stream {
        mode: Mode,
        columns: Vec<Column>,
        sent: Cell<u32>,
        batches: Cell<u32>,
    }

    impl GuestRowStream for Stream {
        fn columns(&self) -> Vec<Column> {
            self.columns.clone()
        }

        fn next_batch(&self, max_rows: u32) -> Result<Option<Vec<Vec<Value>>>, PluginError> {
            let batch = self.batches.get();
            self.batches.set(batch + 1);
            match &self.mode {
                Mode::Rows(total) => {
                    let from = self.sent.get();
                    if from >= *total {
                        return Ok(None);
                    }
                    let to = (*total).min(from + max_rows.max(1));
                    self.sent.set(to);
                    Ok(Some(
                        (from..to)
                            .map(|n| {
                                vec![
                                    Value::Integer(i64::from(n) + 1),
                                    Value::Text(format!("row {}", n + 1)),
                                ]
                            })
                            .collect(),
                    ))
                }
                Mode::Echo(values) => {
                    if batch > 0 {
                        return Ok(None);
                    }
                    Ok(Some(values.iter().map(|v| vec![v.clone()]).collect()))
                }
                Mode::Fixed(rows) => {
                    if batch > 0 {
                        return Ok(None);
                    }
                    Ok(Some(rows.clone()))
                }
                Mode::Spin => loop {
                    std::hint::black_box(());
                },
                Mode::TrapSecond => match batch {
                    0 => Ok(Some(vec![vec![Value::Integer(1)]])),
                    _ => core::arch::wasm32::unreachable(),
                },
                Mode::Wide => Ok(Some(vec![vec![Value::Integer(1), Value::Integer(2)]])),
                Mode::Many => {
                    if batch > 0 {
                        return Ok(None);
                    }
                    Ok(Some((0..501).map(|n| vec![Value::Integer(n)]).collect()))
                }
                Mode::BadDate => {
                    if batch > 0 {
                        return Ok(None);
                    }
                    Ok(Some(vec![vec![Value::Date("2024-13-40".to_owned())]]))
                }
            }
        }
    }

    impl driver_catalog::Guest for Fixture {
        fn supported_kinds(_s: SessionBorrow<'_>) -> Vec<ObjectKind> {
            vec![ObjectKind::Schema, ObjectKind::Table]
        }

        fn list(
            s: SessionBorrow<'_>,
            parent: ObjectPath,
            kind: ObjectKind,
        ) -> Result<Vec<DbObject>, PluginError> {
            let _ = s.get::<Db>();
            let named = |kind: ObjectKind, parent: &ObjectPath, name: &str| DbObject {
                kind,
                parent: ObjectPath {
                    database: parent.database.clone(),
                    schema: parent.schema.clone(),
                    table: parent.table.clone(),
                },
                name: name.to_owned(),
            };
            Ok(match kind {
                ObjectKind::Schema => vec![named(ObjectKind::Schema, &parent, "main")],
                ObjectKind::Table => ["orders", "users"]
                    .iter()
                    .map(|t| named(ObjectKind::Table, &parent, t))
                    .collect(),
                _ => Vec::new(),
            })
        }

        fn columns(
            s: SessionBorrow<'_>,
            table: DbObject,
        ) -> Result<Option<Vec<TableColumn>>, PluginError> {
            let _ = s.get::<Db>();
            Ok(match table.name.as_str() {
                "users" => Some(vec![
                    TableColumn {
                        name: "id".to_owned(),
                        data_type: DataType::Integer,
                        type_name: "INTEGER".to_owned(),
                        nullable: false,
                        default: None,
                    },
                    TableColumn {
                        name: "name".to_owned(),
                        data_type: DataType::Text,
                        type_name: String::new(),
                        nullable: true,
                        default: Some("'anon'".to_owned()),
                    },
                ]),
                _ => None,
            })
        }

        fn definition(
            s: SessionBorrow<'_>,
            object: DbObject,
        ) -> Result<Option<String>, PluginError> {
            let _ = s.get::<Db>();
            Ok(match object.name.as_str() {
                "users" => Some("CREATE TABLE users (id INTEGER NOT NULL, name TEXT)".to_owned()),
                _ => None,
            })
        }
    }

    export!(Fixture);
}
