// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! End-to-end checks of the `MySQL` driver through `dyn` objects only, against the server of
//! `SQLER_TEST_MYSQL_URL` (`MySQL` 8.4 or `MariaDB` 11.8, see `tests/docker/start-mysql.sh`).
//! Without the variable they skip (ADR-0009).
//!
//! One test runs the scenarios one after the other, so the server flavour is detected once.

// Helpers outside `#[test]` fns also unwrap; clippy.toml only exempts the test fns themselves.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]

mod common;

use std::time::{Duration, Instant};

use common::{Flavour, Server, config, connect, run, start};
use sqler_driver_api::sqler_core::{
    ConnectionConfig, DataType, Query, ResultSet, TlsConfig, TlsMode, Value,
};
use sqler_driver_api::{Driver as _, DriverError, QueryEvent, Session, collect};
use sqler_driver_mysql::{DRIVER_ID, MySqlDriver};

async fn query(session: &mut dyn Session, q: &Query) -> Result<ResultSet, DriverError> {
    let mut stream = session.execute(q).await?;
    collect(&mut *stream).await
}

async fn sql(session: &mut dyn Session, text: &str) -> ResultSet {
    query(session, &Query::new(text)).await.unwrap()
}

fn cell<'a>(rs: &'a ResultSet, row: usize, name: &str) -> &'a Value {
    let i = rs.columns().iter().position(|c| c.name == name).unwrap();
    rs.rows()[row].get(i).unwrap()
}

fn column_type(rs: &ResultSet, name: &str) -> DataType {
    rs.columns()
        .iter()
        .find(|c| c.name == name)
        .unwrap()
        .data_type
}

fn date(text: &str) -> Value {
    Value::Date(text.parse().unwrap())
}

fn timestamp(text: &str) -> Value {
    Value::Timestamp(text.parse().unwrap())
}

fn time(text: &str) -> Value {
    Value::Time(text.parse().unwrap())
}

fn decimal(text: &str) -> Value {
    Value::Decimal(text.parse().unwrap())
}

#[test]
fn driver_id() {
    use sqler_driver_api::Driver as _;
    assert_eq!(MySqlDriver::new().id(), "mysql");
    assert_eq!(DRIVER_ID, "mysql");
}

#[test]
fn scenarios_on_server() {
    run(async {
        if let Some(server) = start().await {
            scenarios(&server).await;
        }
    });
}

async fn scenarios(server: &Server) {
    eprintln!("types_round_trip");
    types_round_trip(server).await;
    eprintln!("streams_in_batches");
    streams_in_batches(server).await;
    eprintln!("cancel_kills_server_query");
    cancel_kills_server_query(server).await;
    eprintln!("cancel_statements_without_rows");
    cancel_statements_without_rows(server).await;
    eprintln!("tls_modes");
    tls_modes(server).await;
    eprintln!("rsa_exchange_over_plaintext_cold_cache");
    rsa_exchange_over_plaintext_cold_cache(server).await;
    eprintln!("errors_typed_and_redacted");
    errors_typed_and_redacted(server).await;
    eprintln!("create_procedure_falls_back");
    create_procedure_falls_back(server).await;
}

async fn types_round_trip(server: &Server) {
    let mut s = connect(&server.config()).await;
    sql(&mut *s, "set sql_mode = ''").await;
    sql(&mut *s, "drop table if exists t_types").await;
    sql(&mut *s, "drop table if exists t_params").await;
    sql(
        &mut *s,
        "create table t_types (id int primary key, b tinyint(1), ti tinyint, si smallint, \
         mi mediumint, i int, bi bigint, y year, tu tinyint unsigned, bu bigint unsigned, \
         f float, d double, dc decimal(10,2), c char(3), vc varchar(10), tx text, \
         e enum('a','b'), st set('x','y'), bn binary(2), vb varbinary(4), bl blob, bt bit(8), \
         dt date, dtm datetime(6), ts timestamp(0) null, tm time(6), j json)",
    )
    .await;
    sql(
        &mut *s,
        "insert into t_types values (1, 1, -5, -300, -70000, -2000000000, -9000000000000000000, \
         2024, 255, 18446744073709551615, 1.5, 2.25, 12345.67, 'abc', 'h\u{e9}llo', 'long text', \
         'b', 'x,y', x'0102', x'0a0b', x'ff00', b'10101010', '2024-02-29', \
         '2024-03-04 05:06:07.123456', '2024-03-04 05:06:07', '12:34:56.5', '{\"a\": 1}'), \
         (2, null, null, null, null, null, null, null, null, null, null, null, null, null, \
         null, null, null, null, null, null, null, null, null, null, null, null, null), \
         (3, 0, 127, 32767, 8388607, 2147483647, 9223372036854775807, 1901, 0, 42, 0, 0, \
         -0.5, '', '', '', 'a', '', x'0000', x'', x'', b'0', '0000-00-00', \
         '0000-00-00 00:00:00', null, '-01:00:00', 'null')",
    )
    .await;
    sql(
        &mut *s,
        "insert into t_types (id, tm, dt) values (4, '838:59:59', '2024-00-00'), (5, '25:00:00.000005', '9999-12-31')",
    )
    .await;

    let rs = sql(&mut *s, "select * from t_types order by id").await;
    assert_eq!(rs.len(), 5);
    let json_is_json = server.flavour == Flavour::MySql;

    // Declared types of the result columns.
    let types = [
        ("b", DataType::Boolean),
        ("ti", DataType::Integer),
        ("bi", DataType::Integer),
        ("y", DataType::Integer),
        ("tu", DataType::Integer),
        ("bu", DataType::Integer),
        ("f", DataType::Float),
        ("d", DataType::Float),
        ("dc", DataType::Decimal),
        ("c", DataType::Text),
        ("tx", DataType::Text),
        ("e", DataType::Text),
        ("st", DataType::Text),
        ("bn", DataType::Binary),
        ("bl", DataType::Binary),
        ("bt", DataType::Binary),
        ("dt", DataType::Date),
        ("dtm", DataType::Timestamp),
        ("ts", DataType::Timestamp),
        ("tm", DataType::Time),
        (
            "j",
            if json_is_json {
                DataType::Json
            } else {
                DataType::Binary
            },
        ),
    ];
    for (name, ty) in types {
        assert_eq!(column_type(&rs, name), ty, "{name}");
    }
    assert!(rs.columns().iter().all(|c| c.native_type.is_some()));

    // Row 1: ordinary values.
    let expected = [
        ("b", Value::Bool(true)),
        ("ti", Value::Int(-5)),
        ("si", Value::Int(-300)),
        ("mi", Value::Int(-70000)),
        ("i", Value::Int(-2_000_000_000)),
        ("bi", Value::Int(-9_000_000_000_000_000_000)),
        ("y", Value::Int(2024)),
        ("tu", Value::Int(255)),
        ("bu", decimal("18446744073709551615")),
        ("f", Value::Float(1.5)),
        ("d", Value::Float(2.25)),
        ("dc", decimal("12345.67")),
        ("c", Value::Text("abc".into())),
        ("vc", Value::Text("h\u{e9}llo".into())),
        ("tx", Value::Text("long text".into())),
        ("e", Value::Text("b".into())),
        ("st", Value::Text("x,y".into())),
        ("bn", Value::Bytes(vec![1, 2])),
        ("vb", Value::Bytes(vec![10, 11])),
        ("bl", Value::Bytes(vec![255, 0])),
        ("bt", Value::Bytes(vec![0b1010_1010])),
        ("dt", date("2024-02-29")),
        ("dtm", timestamp("2024-03-04 05:06:07.123456")),
        ("ts", timestamp("2024-03-04 05:06:07")),
        ("tm", time("12:34:56.5")),
    ];
    for (name, value) in expected {
        assert_eq!(cell(&rs, 0, name), &value, "row 1 {name}");
    }
    let json = cell(&rs, 0, "j");
    if json_is_json {
        assert_eq!(json, &Value::Json("{\"a\": 1}".into()));
    } else {
        // `MariaDB` reports its JSON alias (`LONGTEXT` with a binary collation) as a blob.
        assert_eq!(json, &Value::Bytes(b"{\"a\": 1}".to_vec()));
    }

    // Row 2: NULL in every column.
    for column in rs.columns().iter().skip(1) {
        assert_eq!(
            cell(&rs, 1, &column.name),
            &Value::Null,
            "row 2 {}",
            column.name
        );
    }

    // Row 3: extremes, zero dates, a negative TIME.
    assert_eq!(cell(&rs, 2, "b"), &Value::Bool(false));
    assert_eq!(cell(&rs, 2, "bi"), &Value::Int(i64::MAX));
    assert_eq!(cell(&rs, 2, "bu"), &Value::Int(42));
    assert_eq!(cell(&rs, 2, "dt"), &Value::Text("0000-00-00".into()));
    assert_eq!(
        cell(&rs, 2, "dtm"),
        &Value::Text("0000-00-00 00:00:00".into())
    );
    assert_eq!(cell(&rs, 2, "tm"), &Value::Text("-01:00:00".into()));
    assert_eq!(cell(&rs, 2, "vb"), &Value::Bytes(vec![]));

    // Rows 4 and 5: long TIME values, a partly zero date.
    assert_eq!(cell(&rs, 3, "tm"), &Value::Text("838:59:59".into()));
    assert_eq!(cell(&rs, 3, "dt"), &Value::Text("2024-00-00".into()));
    assert_eq!(cell(&rs, 4, "tm"), &Value::Text("25:00:00.000005".into()));
    assert_eq!(cell(&rs, 4, "dt"), &date("9999-12-31"));

    // Unsigned maximum in a plain expression, and NULL.
    let rs = sql(
        &mut *s,
        "select 18446744073709551615 as u, null as n, 1 = 1 as t",
    )
    .await;
    assert_eq!(cell(&rs, 0, "u"), &decimal("18446744073709551615"));
    assert_eq!(cell(&rs, 0, "n"), &Value::Null);

    let rs = sql(&mut *s, "select 'x' collate utf8mb4_bin as vbin").await;
    // The protocol flags a `_bin` collation as binary, so such text arrives as a blob.
    assert_eq!(column_type(&rs, "vbin"), DataType::Binary);
    assert_eq!(cell(&rs, 0, "vbin"), &Value::Bytes(b"x".to_vec()));

    // Geometry is opaque bytes.
    let rs = sql(&mut *s, "select st_geomfromtext('POINT(1 2)') as g").await;
    assert_eq!(column_type(&rs, "g"), DataType::Other);
    assert!(matches!(cell(&rs, 0, "g"), Value::Bytes(b) if !b.is_empty()));

    // The same kinds of values, bound as parameters.
    sql(
        &mut *s,
        "create table t_params (b tinyint(1), i bigint, f double, t varchar(20), bl blob, \
         dc decimal(10,2), d date, tm time(6), ts datetime(6), j json, n int)",
    )
    .await;
    let insert = Query::new("insert into t_params values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)")
        .with_params(vec![
            Value::Bool(true),
            Value::Int(-7),
            Value::Float(1.5),
            Value::Text("h\u{e9}llo".into()),
            Value::Bytes(vec![0, 255]),
            decimal("12.50"),
            date("2024-02-29"),
            time("01:02:03.5"),
            timestamp("2024-03-04 05:06:07.25"),
            Value::Json("{\"k\": [1, 2]}".into()),
            Value::Null,
        ]);
    let done = query(&mut *s, &insert).await.unwrap();
    assert_eq!(done.rows_affected(), Some(1));
    let rs = sql(&mut *s, "select * from t_params").await;
    let expected = [
        ("b", Value::Bool(true)),
        ("i", Value::Int(-7)),
        ("f", Value::Float(1.5)),
        ("t", Value::Text("h\u{e9}llo".into())),
        ("bl", Value::Bytes(vec![0, 255])),
        ("dc", decimal("12.50")),
        ("d", date("2024-02-29")),
        ("tm", time("01:02:03.5")),
        ("ts", timestamp("2024-03-04 05:06:07.25")),
        ("n", Value::Null),
    ];
    for (name, value) in expected {
        assert_eq!(cell(&rs, 0, name), &value, "param {name}");
    }

    // A parameter in a WHERE clause, and a variant the driver cannot bind.
    let rs = query(
        &mut *s,
        &Query::new("select id from t_types where bi = ?").with_params(vec![Value::Int(i64::MAX)]),
    )
    .await
    .unwrap();
    assert_eq!(cell(&rs, 0, "id"), &Value::Int(3));
    s.close().await.unwrap();
}

async fn streams_in_batches(server: &Server) {
    let mut s = connect(&server.config()).await;
    let q = Query::new(
        "with recursive n(i) as (select 1 union all select i + 1 from n where i < 1000) \
         select i from n",
    );
    let mut stream = s.execute(&q).await.unwrap();
    let (mut batches, mut rows, mut columns, mut done) = (0, 0, 0, 0);
    while let Some(event) = stream.next_event().await {
        match event.unwrap() {
            QueryEvent::Columns(_) => columns += 1,
            QueryEvent::Rows(batch) => {
                batches += 1;
                rows += batch.len();
                assert!(batch.len() <= 256);
            }
            QueryEvent::Done { .. } => done += 1,
            other => panic!("unexpected event {other:?}"),
        }
    }
    drop(stream);
    assert_eq!((columns, rows, done), (1, 1000, 1));
    assert!(batches >= 4, "{batches} batches");
    s.close().await.unwrap();
}

/// Cancels `text` after 500 ms and expects `Cancelled`, quickly, with the session still usable.
async fn cancelled_after_half_a_second(session: &mut dyn Session, text: &str) {
    let handle = session.cancel_handle();
    let killer = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        handle.cancel().await
    });
    let started = Instant::now();
    let err = query(session, &Query::new(text)).await.unwrap_err();
    assert!(matches!(err, DriverError::Cancelled), "{text}: {err:?}");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{text}: {:?}",
        started.elapsed()
    );
    killer.await.unwrap().unwrap();
    let rs = sql(session, "select 1 as one").await;
    assert_eq!(rs.len(), 1);
}

async fn cancel_statements_without_rows(server: &Server) {
    let mut s = connect(&server.config()).await;
    sql(&mut *s, "drop table if exists t_cancel").await;
    sql(&mut *s, "create table t_cancel (id int primary key)").await;
    sql(&mut *s, "insert into t_cancel values (1)").await;

    cancelled_after_half_a_second(&mut *s, "do sleep(30)").await;
    cancelled_after_half_a_second(&mut *s, "update t_cancel set id = id where sleep(30) = 0").await;

    // A cancel while idle must not hit the next statement.
    s.cancel_handle().cancel().await.unwrap();
    let rs = sql(&mut *s, "select 1 as one").await;
    assert_eq!(rs.len(), 1);
    sql(&mut *s, "drop table t_cancel").await;
    s.close().await.unwrap();
}

async fn cancel_kills_server_query(server: &Server) {
    let mut victim = connect(&server.config()).await;
    let mut admin = connect(&server.config()).await;
    let rs = sql(&mut *victim, "select connection_id() as id").await;
    let Value::Int(id) = *cell(&rs, 0, "id") else {
        panic!("connection id is not an integer");
    };

    let handle = victim.cancel_handle();
    let killer = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        handle.cancel().await
    });
    let started = Instant::now();
    let err = query(&mut *victim, &Query::new("select sleep(30)"))
        .await
        .unwrap_err();
    assert!(matches!(err, DriverError::Cancelled), "{err:?}");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
    killer.await.unwrap().unwrap();

    // The statement is gone from the server, and the session still works.
    let rs = query(
        &mut *admin,
        &Query::new(
            "select count(*) as n from information_schema.PROCESSLIST \
             where ID = ? and INFO like '%sleep(30)%'",
        )
        .with_params(vec![Value::Int(id)]),
    )
    .await
    .unwrap();
    assert_eq!(cell(&rs, 0, "n"), &Value::Int(0));
    let rs = sql(&mut *victim, "select 1 as one").await;
    assert_eq!(rs.len(), 1);
    victim.close().await.unwrap();
    admin.close().await.unwrap();
}

async fn cipher(session: &mut dyn Session) -> String {
    let rs = sql(session, "show session status like 'Ssl_cipher'").await;
    match cell(&rs, 0, "Value") {
        Value::Text(text) => text.clone(),
        other => panic!("unexpected {other:?}"),
    }
}

fn with_tls(url: &str, mode: TlsMode) -> ConnectionConfig {
    config(url).with_tls(TlsConfig::new(mode))
}

async fn tls_modes(server: &Server) {
    let url = server.url();
    // The first connection (full authentication) happens over TLS, so the server caches the
    // account and a later plain connection needs no RSA exchange.
    let mut s = connect(&with_tls(&url, TlsMode::Require)).await;
    assert!(!cipher(&mut *s).await.is_empty(), "require");
    s.close().await.unwrap();

    let mut s = connect(&with_tls(&url, TlsMode::Disable)).await;
    assert!(cipher(&mut *s).await.is_empty(), "disable");
    s.close().await.unwrap();

    // The URL parameters apply when the config has no TLS options.
    let url_required = server.url_for("root", common::PASSWORD, "ssl-mode=required");
    let mut s = connect(&config(&url_required)).await;
    assert!(!cipher(&mut *s).await.is_empty(), "url ssl-mode=required");
    s.close().await.unwrap();

    // The test CA is not in the system trust store, so verification without it fails.
    let err = MySqlDriver::new()
        .connect(&with_tls(&url, TlsMode::VerifyFull))
        .await
        .err()
        .unwrap();
    assert!(matches!(err, DriverError::Connection(_)), "{err:?}");
    assert!(!err.to_string().contains(common::PASSWORD));

    let Some(ca) = &server.ca else {
        println!("skipped: SQLER_TEST_MYSQL_CA is not set (see AGENTS.md §3)");
        return;
    };
    let with_ca = |url: &str, mode| config(url).with_tls(TlsConfig::new(mode).with_ca_cert(ca));
    // The certificate names `localhost` only, so `127.0.0.1` passes verify-ca and fails
    // verify-full.
    let ip = url.clone();
    let localhost = url.replace("@127.0.0.1", "@localhost");
    assert_ne!(ip, localhost, "the test URL must use 127.0.0.1");
    let mut s = connect(&with_ca(&ip, TlsMode::VerifyCa)).await;
    assert!(!cipher(&mut *s).await.is_empty(), "verify-ca");
    s.close().await.unwrap();
    let mut s = connect(&with_ca(&localhost, TlsMode::VerifyFull)).await;
    assert!(!cipher(&mut *s).await.is_empty(), "verify-full");
    s.close().await.unwrap();
    let err = MySqlDriver::new()
        .connect(&with_ca(&ip, TlsMode::VerifyFull))
        .await
        .err()
        .unwrap();
    assert!(matches!(err, DriverError::Connection(_)), "{err:?}");
}

/// A fresh `caching_sha2_password` account has no entry in the server cache, so a plaintext login
/// needs the RSA public-key exchange (the vendored `sqlx-mysql` backend on `aws-lc-rs`).
async fn rsa_exchange_over_plaintext_cold_cache(server: &Server) {
    if server.flavour != Flavour::MySql {
        println!("skipped: MariaDB has no caching_sha2_password (no RSA exchange)");
        return;
    }
    let mut admin = connect(&server.config()).await;
    sql(&mut *admin, "drop user if exists 'sqler_rsa'@'%'").await;
    sql(
        &mut *admin,
        "create user 'sqler_rsa'@'%' identified with caching_sha2_password by 'rsa-pass'",
    )
    .await;
    sql(&mut *admin, "grant all on sqler.* to 'sqler_rsa'@'%'").await;
    // Empties the authentication cache, so the first login must use the full exchange.
    sql(&mut *admin, "flush privileges").await;

    let url = server.url_for("sqler_rsa", "rsa-pass", "ssl-mode=disabled");
    let mut s = connect(&config(&url)).await;
    assert!(cipher(&mut *s).await.is_empty(), "the link must be plain");
    assert_eq!(sql(&mut *s, "select 1 as one").await.len(), 1);
    s.close().await.unwrap();

    // Downgrade protection: an attacker who strips the server's TLS offer must not obtain the
    // password through the in-band RSA key. `prefer` (the default) refuses the plaintext
    // exchange; only an explicit disable accepts it.
    let strip = tls_stripping_proxy(server.addr()).await;
    for query in ["", "ssl-mode=preferred"] {
        let url = proxied(&server.url_for("sqler_rsa", "rsa-pass", query), &strip);
        let err = MySqlDriver::new()
            .connect(&config(&url))
            .await
            .err()
            .unwrap();
        assert!(matches!(err, DriverError::Connection(_)), "{err:?}");
        assert!(
            err.to_string().contains(
                "plaintext RSA password exchange needs an explicit ssl-mode=disabled; use TLS"
            ),
            "{err}"
        );
        assert!(!format!("{err} {err:?}").contains("rsa-pass"));
    }
    // The same through the typed TLS mode of the connection config.
    let url = proxied(&server.url_for("sqler_rsa", "rsa-pass", ""), &strip);
    let err = MySqlDriver::new()
        .connect(&config(&url).with_tls(TlsConfig::new(TlsMode::Prefer)))
        .await
        .err()
        .unwrap();
    assert!(
        err.to_string().contains("explicit ssl-mode=disabled"),
        "{err}"
    );
    // `require` is unchanged: with no TLS offered it fails on TLS, not on the RSA exchange.
    let url = proxied(
        &server.url_for("sqler_rsa", "rsa-pass", "ssl-mode=required"),
        &strip,
    );
    let err = MySqlDriver::new()
        .connect(&config(&url))
        .await
        .err()
        .unwrap();
    assert!(err.to_string().contains("does not support TLS"), "{err}");
    // An explicit disable still works through the same proxy.
    let url = proxied(
        &server.url_for("sqler_rsa", "rsa-pass", "ssl-mode=disabled"),
        &strip,
    );
    let mut s = connect(&config(&url)).await;
    assert_eq!(sql(&mut *s, "select 1 as one").await.len(), 1);
    s.close().await.unwrap();

    // A wrong password still fails with a typed, redacted error.
    let bad = server.url_for("sqler_rsa", "rsa-wrong-1234", "ssl-mode=disabled");
    let err = MySqlDriver::new()
        .connect(&config(&bad))
        .await
        .err()
        .unwrap();
    assert!(matches!(err, DriverError::Connection(_)), "{err:?}");
    assert!(!format!("{err} {err:?}").contains("rsa-wrong-1234"));

    sql(&mut *admin, "drop user 'sqler_rsa'@'%'").await;
    admin.close().await.unwrap();
}

/// Replaces the `host:port` of `url` with the proxy address.
fn proxied(url: &str, proxy: &str) -> String {
    let (head, rest) = url.split_once('@').unwrap();
    let tail = rest.find(['/', '?']).map_or("", |i| &rest[i..]);
    format!("{head}@{proxy}{tail}")
}

/// A TCP proxy to `upstream` that clears the `CLIENT_SSL` capability in the server's handshake,
/// like an active attacker stripping the TLS offer. Returns its `127.0.0.1:port`.
async fn tls_stripping_proxy(upstream: String) -> String {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::{TcpListener, TcpStream};

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        while let Ok((client, _)) = listener.accept().await {
            let upstream = upstream.clone();
            tokio::spawn(async move {
                let Ok(server) = TcpStream::connect(upstream).await else {
                    return;
                };
                let (mut cr, mut cw) = client.into_split();
                let (mut sr, mut sw) = server.into_split();
                tokio::spawn(async move {
                    let _ = tokio::io::copy(&mut cr, &mut sw).await;
                });
                // The first packet is the handshake: header(4), protocol(1), version(NUL),
                // connection id(4), scramble(8), filler(1), capabilities(2, low bytes first).
                let mut header = [0u8; 4];
                if sr.read_exact(&mut header).await.is_err() {
                    return;
                }
                let len = usize::from(header[0])
                    | usize::from(header[1]) << 8
                    | usize::from(header[2]) << 16;
                let mut payload = vec![0u8; len];
                if sr.read_exact(&mut payload).await.is_err() {
                    return;
                }
                let nul = payload.iter().position(|b| *b == 0).unwrap();
                payload[nul + 1 + 4 + 8 + 1 + 1] &= !0x08; // CLIENT_SSL is bit 11
                let _ = cw.write_all(&header).await;
                let _ = cw.write_all(&payload).await;
                let _ = tokio::io::copy(&mut sr, &mut cw).await;
            });
        }
    });
    addr
}

async fn errors_typed_and_redacted(server: &Server) {
    let url = server.url_for("root", "wrong-pw-1234", "");
    let err = MySqlDriver::new()
        .connect(&config(&url))
        .await
        .err()
        .unwrap();
    assert!(matches!(err, DriverError::Connection(_)), "{err:?}");
    let text = format!("{err} {err:?}");
    assert!(!text.contains("wrong-pw-1234"), "{text}");
    assert!(!text.contains(&url), "{text}");

    let mut s = connect(&server.config()).await;
    let err = query(&mut *s, &Query::new("selec 1")).await.unwrap_err();
    assert!(matches!(err, DriverError::Query(_)), "{err:?}");
    // The session survives a failed statement.
    assert_eq!(sql(&mut *s, "select 1 as one").await.len(), 1);
    s.close().await.unwrap();
}

async fn create_procedure_falls_back(server: &Server) {
    let mut s = connect(&server.config()).await;
    sql(&mut *s, "drop procedure if exists p_fallback").await;
    let rs = sql(&mut *s, "create procedure p_fallback() begin select 1; end").await;
    assert!(rs.rows_affected().is_some());
    assert_eq!(rs.columns().len(), 0);
    let rs = sql(
        &mut *s,
        "select count(*) as n from information_schema.ROUTINES where ROUTINE_NAME = 'p_fallback'",
    )
    .await;
    assert_eq!(cell(&rs, 0, "n"), &Value::Int(1));
    s.close().await.unwrap();
}
