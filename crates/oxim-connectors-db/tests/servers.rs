//! PostgreSQL, MySQL and SQL Server round trips against real servers.
//!
//! Each test runs only when its environment variable holds the connection
//! settings as a YAML flow mapping, for example:
//!
//! ```text
//! OXIM_TEST_POSTGRES="{host: 127.0.0.1, database: oxim, user: oxim, password: secret}"
//! OXIM_TEST_MYSQL="{host: 127.0.0.1, database: oxim, user: oxim, password: secret}"
//! OXIM_TEST_MSSQL="{host: 127.0.0.1, database: oxim, user: sa, password: secret, encryption: none}"
//! ```
//!
//! The tests create and drop their own table.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use oxim_core::{
    ChannelConfig, DestinationConnector, Engine, EngineOptions, Registry, SendError, SystemClock,
    async_trait,
};
use oxim_model::{ChannelId, ConnectorId, DataType, MessageId};
use oxim_store::{Delivery, SqliteStore};

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

struct Dialect {
    kind: &'static str,
    variable: &'static str,
    create: &'static str,
    taken: &'static str,
    expected_taken: &'static str,
    unsent: &'static str,
    set_sent: &'static str,
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

fn writer(
    registry: &Registry,
    dialect: &Dialect,
    connection: &str,
    statement: &str,
    params: &str,
) -> Arc<dyn DestinationConnector> {
    let config = ChannelConfig::from_yaml(&format!(
        "id: t\nsource: {{type: timer, data_type: raw, settings: {{interval: 1h}}}}\ndestinations:\n  - id: db\n    type: {}\n    settings: {}\n",
        dialect.kind,
        merge(connection, &format!("statement: \"{statement}\", params: {params}"))
    ))
    .unwrap();
    registry.destination(&config.destinations[0]).unwrap()
}

/// Adds settings to the connection mapping.
fn merge(connection: &str, extra: &str) -> String {
    let inner = connection
        .trim()
        .trim_start_matches('{')
        .trim_end_matches('}');
    format!("{{{inner}, {extra}}}")
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

async fn round_trip(dialect: Dialect) {
    let Ok(connection) = std::env::var(dialect.variable) else {
        eprintln!("{} is not set; skipping", dialect.variable);
        return;
    };
    let table = format!("oxim_test_{}", std::process::id());
    let recorder = Arc::new(Recorder::default());
    let registry = registry(recorder.clone());

    let create = writer(
        &registry,
        &dialect,
        &connection,
        &dialect.create.replace("TABLE_NAME", &table),
        "{}",
    );
    create.send(&delivery(0, HL7)).await.unwrap();

    let insert = writer(
        &registry,
        &dialect,
        &connection,
        &format!(
            "INSERT INTO {table} (specimen, glucose, taken) VALUES (:specimen, :glucose, :taken)"
        ),
        &format!(
            "{{specimen: PID-3.1, glucose: {{value: '5.40'}}, taken: {{value: '{}'}}}}",
            dialect.taken
        ),
    );
    insert.send(&delivery(1, HL7)).await.unwrap();
    // A NULL specimen violates NOT NULL: rejected, not retried.
    let error = insert
        .send(&delivery(
            2,
            b"MSH|^~\\&|LAB|HOSP|LIS|HOSP|20260929120000||ORU^R01|C43|P|2.5.1\r",
        ))
        .await
        .unwrap_err();
    assert!(error.permanent, "{error}");

    let mut options = EngineOptions::default();
    options.idle_poll = Duration::from_millis(20);
    let engine = Engine::start(
        Box::new(SqliteStore::open_in_memory().unwrap()),
        registry,
        Arc::new(SystemClock),
        options,
    )
    .await
    .unwrap();
    let source = merge(
        &connection,
        &format!(
            "query: \"SELECT id, specimen, glucose, taken FROM {table} WHERE {} ORDER BY id\", post_query: \"UPDATE {table} SET {} WHERE id = :id\", interval: 200ms",
            dialect.unsent, dialect.set_sent
        ),
    );
    engine
        .deploy(
            ChannelConfig::from_yaml(&format!(
                "id: reader\nsource: {{type: {}, data_type: json, settings: {source}}}\ndestinations:\n  - {{id: out, type: recorder}}\n",
                dialect.kind
            ))
            .unwrap(),
        )
        .await
        .unwrap();
    for _ in 0..250 {
        if !recorder.sent.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let row: serde_json::Value =
        serde_json::from_slice(recorder.sent.lock().unwrap().first().expect("no row")).unwrap();
    assert_eq!(row["specimen"], "P1001");
    assert_eq!(row["glucose"], "5.40");
    assert_eq!(row["taken"], dialect.expected_taken);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(recorder.sent.lock().unwrap().len(), 1, "the row was marked");
    engine.shutdown().await;

    let drop = writer(
        &registry_for_drop(),
        &dialect,
        &connection,
        &format!("DROP TABLE {table}"),
        "{}",
    );
    drop.send(&delivery(3, HL7)).await.unwrap();
}

fn registry_for_drop() -> Registry {
    registry(Arc::new(Recorder::default()))
}

#[tokio::test(flavor = "multi_thread")]
async fn postgres_round_trip() {
    round_trip(Dialect {
        kind: "postgres",
        variable: "OXIM_TEST_POSTGRES",
        create: "CREATE TABLE TABLE_NAME (id serial PRIMARY KEY, specimen text NOT NULL, glucose numeric(6,2), taken timestamptz, sent boolean NOT NULL DEFAULT false)",
        taken: "2026-09-29T12:00:00Z",
        expected_taken: "2026-09-29T12:00:00Z",
        unsent: "sent = false",
        set_sent: "sent = true",
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn mysql_round_trip() {
    round_trip(Dialect {
        kind: "mysql",
        variable: "OXIM_TEST_MYSQL",
        create: "CREATE TABLE TABLE_NAME (id INT AUTO_INCREMENT PRIMARY KEY, specimen VARCHAR(32) NOT NULL, glucose DECIMAL(6,2), taken DATETIME(3), sent TINYINT NOT NULL DEFAULT 0)",
        taken: "2026-09-29 12:00:00",
        expected_taken: "2026-09-29T12:00:00",
        unsent: "sent = 0",
        set_sent: "sent = 1",
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn mssql_round_trip() {
    round_trip(Dialect {
        kind: "mssql",
        variable: "OXIM_TEST_MSSQL",
        create: "CREATE TABLE TABLE_NAME (id INT IDENTITY PRIMARY KEY, specimen NVARCHAR(32) NOT NULL, glucose DECIMAL(6,2), taken DATETIME2(3), sent BIT NOT NULL DEFAULT 0)",
        taken: "2026-09-29 12:00:00",
        expected_taken: "2026-09-29T12:00:00",
        unsent: "sent = 0",
        set_sent: "sent = 1",
    })
    .await;
}
