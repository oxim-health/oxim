//! The polling reader and the writer end to end, on a SQLite file.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use oxim_core::{
    ChannelConfig, DestinationConnector, Engine, EngineOptions, Registry, SendError, SystemClock,
    async_trait,
};
use oxim_model::{ChannelId, ConnectorId, DataType, MessageId};
use oxim_store::{Delivery, SqliteStore};

/// A destination that records every payload.
#[derive(Debug, Default)]
struct Recorder {
    sent: Mutex<Vec<Vec<u8>>>,
}

#[async_trait]
impl DestinationConnector for Recorder {
    async fn send(&self, delivery: &Delivery) -> Result<Option<Vec<u8>>, SendError> {
        self.sent.lock().unwrap().push(delivery.payload.clone());
        Ok(None)
    }
}

fn registry(recorder: Arc<Recorder>) -> Registry {
    let mut registry = Registry::new();
    oxim_connectors::register(&mut registry);
    oxim_connectors_db::register(&mut registry);
    registry.add_destination("recorder", move |_| {
        Ok(recorder.clone() as Arc<dyn DestinationConnector>)
    });
    registry
}

async fn engine(recorder: Arc<Recorder>) -> Engine {
    let mut options = EngineOptions::default();
    options.idle_poll = Duration::from_millis(20);
    options.shutdown_grace = Duration::from_secs(5);
    Engine::start(
        Box::new(SqliteStore::open_in_memory().unwrap()),
        registry(recorder),
        Arc::new(SystemClock),
        options,
    )
    .await
    .unwrap()
}

fn yaml_path(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\\', "/"))
}

async fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    for _ in 0..500 {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting until {what}");
}

fn query(path: &Path, sql: &str) -> Vec<Vec<String>> {
    let conn = rusqlite::Connection::open(path).unwrap();
    let mut stmt = conn.prepare(sql).unwrap();
    let columns = stmt.column_count();
    stmt.query_map([], |row| {
        (0..columns)
            .map(|i| {
                let value: rusqlite::types::Value = row.get(i)?;
                Ok(match value {
                    rusqlite::types::Value::Null => "NULL".to_owned(),
                    rusqlite::types::Value::Integer(n) => n.to_string(),
                    rusqlite::types::Value::Real(x) => x.to_string(),
                    rusqlite::types::Value::Text(t) => t,
                    rusqlite::types::Value::Blob(b) => format!("blob:{}", b.len()),
                })
            })
            .collect()
    })
    .unwrap()
    .collect::<Result<_, _>>()
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn reads_rows_and_marks_them_processed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lis.sqlite");
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE orders (id INTEGER PRIMARY KEY, specimen TEXT NOT NULL, test TEXT, \
             glucose TEXT, raw BLOB, sent INTEGER NOT NULL DEFAULT 0);
             INSERT INTO orders (specimen, test, glucose, raw) VALUES
               ('S1', 'GLU', '5.40', x'00ff'), ('S2', 'HGB', NULL, NULL), ('S3', 'GLU', '6.10', NULL);",
        )
        .unwrap();
    }
    let recorder = Arc::new(Recorder::default());
    let engine = engine(recorder.clone()).await;
    let config = ChannelConfig::from_yaml(&format!(
        "id: orders
source:
  type: sqlite
  data_type: json
  settings:
    path: {}
    query: SELECT id, specimen, test, glucose, raw FROM orders WHERE sent = 0 AND test <> :skip ORDER BY id
    params: {{skip: CREA}}
    post_query: UPDATE orders SET sent = 1 WHERE id = :id
    interval: 100ms
    max_rows: 2
destinations:
  - {{id: out, type: recorder}}
",
        yaml_path(&path)
    ))
    .unwrap();
    engine.deploy(config).await.unwrap();
    wait_until("three rows are read", || {
        recorder.sent.lock().unwrap().len() == 3
    })
    .await;
    let first: serde_json::Value =
        serde_json::from_slice(&recorder.sent.lock().unwrap()[0]).unwrap();
    assert_eq!(
        serde_json::to_string(&first).unwrap(),
        r#"{"id":1,"specimen":"S1","test":"GLU","glucose":"5.40","raw":"AP8="}"#
    );
    assert_eq!(query(&path, "SELECT sum(sent) FROM orders"), [["3"]]);

    // New rows are picked up; rows the query excludes are not.
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "INSERT INTO orders (specimen, test) VALUES ('S4', 'CREA'), ('S5', 'NA');",
        )
        .unwrap();
    }
    wait_until("the new row is read", || {
        recorder.sent.lock().unwrap().len() == 4
    })
    .await;
    let last: serde_json::Value =
        serde_json::from_slice(&recorder.sent.lock().unwrap()[3]).unwrap();
    assert_eq!(last["specimen"], "S5");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(recorder.sent.lock().unwrap().len(), 4);
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_column_can_be_the_message() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("outbox.sqlite");
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE outbox (id INTEGER PRIMARY KEY, message TEXT, done INTEGER DEFAULT 0);
             INSERT INTO outbox (message) VALUES ('MSH|^~\\&|HIS|H|OXIM|H|20260929120000||ADT^A01|A1|P|2.5.1');",
        )
        .unwrap();
    }
    let recorder = Arc::new(Recorder::default());
    let engine = engine(recorder.clone()).await;
    let config = ChannelConfig::from_yaml(&format!(
        "id: outbox
source:
  type: sqlite
  data_type: hl7v2
  settings:
    path: {}
    query: SELECT id, message FROM outbox WHERE done = 0
    post_query: UPDATE outbox SET done = 1 WHERE id = :id
    column: message
    interval: 100ms
destinations:
  - {{id: out, type: recorder}}
",
        yaml_path(&path)
    ))
    .unwrap();
    engine.deploy(config).await.unwrap();
    wait_until("the message is read", || {
        recorder.sent.lock().unwrap().len() == 1
    })
    .await;
    assert!(recorder.sent.lock().unwrap()[0].starts_with(b"MSH|^~\\&|HIS|"));
    engine.shutdown().await;
}

fn delivery(n: u64, payload: &[u8]) -> Delivery {
    Delivery {
        message_id: MessageId::from_parts(1_790_000_000_000 + n, u128::from(n)),
        channel: ChannelId::new("lab").unwrap(),
        destination: ConnectorId::new("db").unwrap(),
        attempts: 0,
        payload: payload.to_vec(),
        data_type: Some(DataType::Hl7V2),
    }
}

const HL7: &[u8] = b"MSH|^~\\&|LAB|HOSP|LIS|HOSP|20260929120000||ORU^R01|C42|P|2.5.1\rPID|1||P1001^^^LAB||DOE^JANE\r";

#[tokio::test(flavor = "multi_thread")]
async fn writes_deliveries_in_transactions() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("archive.sqlite");
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE results (message_id TEXT PRIMARY KEY, control TEXT NOT NULL, mrn TEXT, \
             body TEXT, raw BLOB, site TEXT);
             CREATE TABLE counts (site TEXT PRIMARY KEY, n INTEGER NOT NULL);
             INSERT INTO counts VALUES ('LAB1', 0);",
        )
        .unwrap();
    }
    let config = ChannelConfig::from_yaml(&format!(
        "id: lab
source: {{type: timer, data_type: hl7v2, settings: {{interval: 1h}}}}
destinations:
  - id: db
    type: sqlite
    settings:
      path: {}
      statement:
        - INSERT INTO results (message_id, control, mrn, body, raw, site) VALUES (:id, :control, :mrn, :body, :raw, :site)
        - UPDATE counts SET n = n + 1 WHERE site = :site
      params:
        id: $message_id
        control: MSH-10
        mrn: PID-3.1
        body: $payload
        raw: $payload_bytes
        site: {{value: LAB1}}
",
        yaml_path(&path)
    ))
    .unwrap();
    let destination = registry(Arc::new(Recorder::default()))
        .destination(&config.destinations[0])
        .unwrap();
    let answer = destination.send(&delivery(1, HL7)).await.unwrap();
    assert_eq!(answer.as_deref(), Some(&b"2 rows affected"[..]));
    let rows = query(&path, "SELECT control, mrn, site, raw FROM results");
    assert_eq!(
        rows,
        [["C42", "P1001", "LAB1", &format!("blob:{}", HL7.len())]]
    );
    assert_eq!(query(&path, "SELECT n FROM counts"), [["1"]]);

    // The same message again violates the primary key: a permanent failure,
    // and the transaction leaves the counter untouched.
    let error = destination.send(&delivery(1, HL7)).await.unwrap_err();
    assert!(error.permanent, "{error}");
    assert!(error.message.contains("UNIQUE"), "{error}");
    assert_eq!(query(&path, "SELECT n FROM counts"), [["1"]]);

    // A message without the required value fails permanently too.
    let error = destination
        .send(&delivery(
            2,
            b"MSH|^~\\&|LAB|HOSP|LIS|HOSP|20260929120000||ORU^R01||P|2.5.1\r",
        ))
        .await
        .unwrap_err();
    assert!(
        error.permanent && error.message.contains("NOT NULL"),
        "{error}"
    );

    // A payload that does not parse cannot be bound.
    let error = destination
        .send(&delivery(3, b"garbage"))
        .await
        .unwrap_err();
    assert!(error.permanent, "{error}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_database_is_retried() {
    let dir = tempfile::tempdir().unwrap();
    let config = ChannelConfig::from_yaml(&format!(
        "id: lab
source: {{type: timer, data_type: hl7v2, settings: {{interval: 1h}}}}
destinations:
  - id: db
    type: sqlite
    settings:
      path: {}
      statement: INSERT INTO t (id) VALUES (:id)
      params: {{id: $message_id}}
",
        yaml_path(&dir.path().join("missing.sqlite"))
    ))
    .unwrap();
    let destination = registry(Arc::new(Recorder::default()))
        .destination(&config.destinations[0])
        .unwrap();
    let error = destination.send(&delivery(1, HL7)).await.unwrap_err();
    assert!(!error.permanent, "{error}");
}

#[test]
fn settings_are_validated_at_deploy() {
    let registry = registry(Arc::new(Recorder::default()));
    for source in [
        // Unknown key.
        "{type: sqlite, data_type: json, settings: {path: x.db, query: SELECT 1, colour: red}}",
        // A query parameter without a value.
        "{type: sqlite, data_type: json, settings: {path: x.db, query: 'SELECT :a'}}",
        // Positional placeholders.
        "{type: sqlite, data_type: json, settings: {path: x.db, query: 'SELECT ?'}}",
        // Both password forms.
        "{type: postgres, data_type: json, settings: {host: h, database: d, user: u, password: a, password_env: B, query: SELECT 1}}",
        "{type: mysql, data_type: json, settings: {host: h, user: u, query: SELECT 1}}",
        "{type: mssql, data_type: json, settings: {host: h, database: d, user: u, query: SELECT 1, encryption: sometimes}}",
    ] {
        let config = ChannelConfig::from_yaml(&format!("id: x\nsource: {source}\n")).unwrap();
        assert!(registry.source(&config.source).is_err(), "{source}");
    }
    for settings in [
        "{path: x.db, statement: 'INSERT INTO t VALUES (:a)'}",
        "{path: x.db, statement: 'INSERT INTO t VALUES (1)', params: {a: $payload}}",
        "{path: x.db, statement: 'INSERT INTO t VALUES (:a)', params: {a: $nothing}}",
    ] {
        let config = ChannelConfig::from_yaml(&format!(
            "id: x\nsource: {{type: timer, data_type: raw, settings: {{interval: 1s}}}}\ndestinations:\n  - {{id: d, type: sqlite, settings: {settings}}}\n"
        ))
        .unwrap();
        assert!(
            registry.destination(&config.destinations[0]).is_err(),
            "{settings}"
        );
    }
    let ok = ChannelConfig::from_yaml(
        "id: x\nsource: {type: postgres, data_type: json, settings: {host: h, database: d, user: u, password_env: OXIM_PG_PASSWORD, query: 'SELECT * FROM t WHERE a = :a', params: {a: 1}}}\n",
    )
    .unwrap();
    assert!(registry.source(&ok.source).is_ok());
}
