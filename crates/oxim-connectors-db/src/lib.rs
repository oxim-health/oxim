//! Database connectors for OXIM channels.
//!
//! | Type | Database | Source | Destination |
//! |---|---|---|---|
//! | `postgres` | PostgreSQL | polling reader | writer |
//! | `mysql` | MySQL, MariaDB | polling reader | writer |
//! | `mssql` | Microsoft SQL Server, Azure SQL | polling reader | writer |
//! | `sqlite` | SQLite database files | polling reader | writer |
//!
//! Statements use named parameters (`:name`) for every database; OXIM
//! rewrites them to the database's placeholders and binds every value as a
//! parameter, never by editing the SQL text.
//!
//! **Polling reader (source).** Every `interval` the `query` runs (with the
//! constant `params`), and each returned row becomes one message: the row
//! as a JSON object (column name to value; exact numerics as text, dates
//! and times in ISO 8601, binary values in Base64), or with `column` the
//! raw content of one column. After a row is stored durably, `post_query`
//! runs with the row's columns as parameters, typically to mark the row as
//! processed. A crash between the two reads the row again: delivery is
//! at-least-once, never lossy.
//!
//! ```yaml
//! source:
//!   type: postgres
//!   data_type: json
//!   settings:
//!     host: lis-db.hospital.internal
//!     database: lis
//!     user: oxim
//!     password_env: OXIM_LIS_DB_PASSWORD
//!     tls: {ca_file: /etc/oxim/tls/hospital-ca.pem}
//!     query: SELECT id, specimen, test, requested_at FROM orders WHERE site = :site AND sent = false ORDER BY id
//!     params: {site: LAB1}
//!     post_query: UPDATE orders SET sent = true WHERE id = :id
//!     interval: 10s
//! ```
//!
//! **Writer (destination).** For each delivery the `statement` (or a list
//! of statements) runs in one transaction. Parameters come from the
//! delivery: `$payload`, `$payload_bytes` (binary), `$message_id`,
//! `$channel`, `$destination`, `$data_type`, `$attempts`, constants
//! (`{value: ...}`) or paths into the delivered message (`MSH-10`,
//! `PID-3.1`, `order.id`); see [`oxim_connectors::values`]. Connection
//! failures and other database errors are retried; constraint and data
//! errors fail the delivery, because repeating the statement cannot
//! succeed.
//!
//! ```yaml
//! destinations:
//!   - id: archive-db
//!     type: mssql
//!     settings:
//!       host: sql.hospital.internal
//!       database: integration
//!       user: oxim
//!       password_env: OXIM_SQL_PASSWORD
//!       statement: INSERT INTO lab_results (message_id, control_id, mrn, body) VALUES (:id, :control, :mrn, :body)
//!       params: {id: $message_id, control: MSH-10, mrn: PID-3.1, body: $payload}
//! ```
//!
//! Passwords come from `password_env` (an environment variable read when
//! connecting) or `password`; connection strings are never logged. TLS uses
//! rustls with the ring provider; see the crate README for each database's
//! connection settings. ODBC and Oracle are not supported yet: both need
//! native client libraries.

mod connector;
mod mssql;
mod mysql;
mod postgres;
mod settings;
mod sql;
mod sqlite;
mod value;

use std::sync::Arc;

use oxim_core::{
    DestinationConfig, DestinationConnector, EngineError, Registry, Settings, SourceConfig,
    SourceConnector,
};

use crate::connector::{Database, DbDestination, DbSource};
use crate::settings::{Reader, ReaderSettings, Writer, WriterSettings, parse, split};

fn source<D: Database>(
    settings: &Settings,
    keys: &[&str],
    open: fn(Settings) -> Result<D, EngineError>,
    kind: &str,
) -> Result<Arc<dyn SourceConnector>, EngineError> {
    let (connection, rest) = split(settings, keys);
    let db = open(connection)?;
    let reader: ReaderSettings = parse(rest, &format!("{kind} source"))?;
    let reader = Reader::new(reader, db.style())?;
    Ok(Arc::new(DbSource::new(db, reader)))
}

fn destination<D: Database>(
    settings: &Settings,
    keys: &[&str],
    open: fn(Settings) -> Result<D, EngineError>,
    kind: &str,
) -> Result<Arc<dyn DestinationConnector>, EngineError> {
    let (connection, rest) = split(settings, keys);
    let db = open(connection)?;
    let writer: WriterSettings = parse(rest, &format!("{kind} destination"))?;
    let writer = Writer::new(writer, db.style())?;
    Ok(Arc::new(DbDestination::new(db, writer)))
}

/// Registers `postgres`, `mysql`, `mssql` and `sqlite` as sources and
/// destinations.
pub fn register(registry: &mut Registry) {
    registry
        .add_source("postgres", |config: &SourceConfig| {
            source(
                &config.settings,
                postgres::KEYS,
                postgres::Postgres::from_settings,
                "postgres",
            )
        })
        .add_destination("postgres", |config: &DestinationConfig| {
            destination(
                &config.settings,
                postgres::KEYS,
                postgres::Postgres::from_settings,
                "postgres",
            )
        })
        .add_source("mysql", |config: &SourceConfig| {
            source(
                &config.settings,
                mysql::KEYS,
                mysql::Mysql::from_settings,
                "mysql",
            )
        })
        .add_destination("mysql", |config: &DestinationConfig| {
            destination(
                &config.settings,
                mysql::KEYS,
                mysql::Mysql::from_settings,
                "mysql",
            )
        })
        .add_source("mssql", |config: &SourceConfig| {
            source(
                &config.settings,
                mssql::KEYS,
                mssql::Mssql::from_settings,
                "mssql",
            )
        })
        .add_destination("mssql", |config: &DestinationConfig| {
            destination(
                &config.settings,
                mssql::KEYS,
                mssql::Mssql::from_settings,
                "mssql",
            )
        })
        .add_source("sqlite", |config: &SourceConfig| {
            source(
                &config.settings,
                sqlite::KEYS,
                sqlite::Sqlite::from_settings,
                "sqlite",
            )
        })
        .add_destination("sqlite", |config: &DestinationConfig| {
            destination(
                &config.settings,
                sqlite::KEYS,
                sqlite::Sqlite::from_settings,
                "sqlite",
            )
        });
}
