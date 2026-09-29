//! MySQL and MariaDB.

use std::time::Duration;

use mysql_async::consts::ColumnType;
use mysql_async::prelude::Queryable;
use mysql_async::{ClientIdentity, Conn, OptsBuilder, Params, SslOpts, TxOpts, Value};
use oxim_connectors::tls::{ClientTlsSettings, client_config};
use oxim_core::config::DurationText;
use oxim_core::{EngineError, Settings, async_trait};
use serde::Deserialize;

use crate::connector::{Database, DbError};
use crate::settings::{Secret, parse};
use crate::sql::{Placeholder, Prepared};
use crate::value::{Cell, Param, Row, time_text};

fn default_port() -> u16 {
    3306
}

fn default_connect_timeout() -> DurationText {
    DurationText(Duration::from_secs(10))
}

/// Connection settings of `mysql`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MysqlSettings {
    host: String,
    #[serde(default = "default_port")]
    port: u16,
    database: String,
    user: String,
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    password_env: Option<String>,
    #[serde(default)]
    tls: Option<ClientTlsSettings>,
    #[serde(default = "default_connect_timeout")]
    connect_timeout: DurationText,
}

/// The connection setting names.
pub(crate) const KEYS: &[&str] = &[
    "host",
    "port",
    "database",
    "user",
    "password",
    "password_env",
    "tls",
    "connect_timeout",
];

/// The binary character set: `BLOB` and `VARBINARY` columns.
const BINARY_CHARSET: u16 = 63;

/// A MySQL or MariaDB server.
#[derive(Clone)]
pub(crate) struct Mysql {
    settings: MysqlSettings,
    secret: Secret,
    ssl: Option<SslOpts>,
}

impl std::fmt::Debug for Mysql {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.describe())
    }
}

impl Mysql {
    pub(crate) fn from_settings(settings: Settings) -> Result<Self, EngineError> {
        let settings: MysqlSettings = parse(settings, "mysql connection")?;
        let secret = Secret::new(settings.password.clone(), settings.password_env.clone())?;
        let ssl = settings.tls.as_ref().map(ssl_opts).transpose()?;
        Ok(Self {
            settings,
            secret,
            ssl,
        })
    }
}

/// The driver's TLS options. The files are also read once here so bad
/// paths fail at deploy time. `system_roots` selects the driver's built-in
/// (Mozilla) root certificates.
fn ssl_opts(tls: &ClientTlsSettings) -> Result<SslOpts, EngineError> {
    client_config(tls)?;
    let mut ssl = SslOpts::default().with_disable_built_in_roots(!tls.system_roots);
    if let Some(ca) = &tls.ca_file {
        ssl = ssl.with_root_certs(vec![ca.clone().into()]);
    }
    if let (Some(cert), Some(key)) = (&tls.cert_file, &tls.key_file) {
        ssl = ssl.with_client_identity(Some(ClientIdentity::new(
            cert.clone().into(),
            key.clone().into(),
        )));
    }
    if let Some(name) = &tls.server_name {
        ssl = ssl.with_danger_tls_hostname_override(Some(name.clone()));
    }
    Ok(ssl)
}

/// MySQL server error codes for rejected data: duplicate keys, foreign
/// keys, `NOT NULL`, truncation, out-of-range and invalid values, check
/// constraints.
const REJECTED_CODES: &[u16] = &[
    1048, 1062, 1216, 1217, 1263, 1264, 1265, 1292, 1366, 1406, 1451, 1452, 1586, 3819,
];

fn classify(e: &mysql_async::Error) -> DbError {
    match e {
        mysql_async::Error::Server(server) if REJECTED_CODES.contains(&server.code) => {
            DbError::rejected(format!("{} (error {})", server.message, server.code))
        }
        mysql_async::Error::Server(server) => {
            DbError::other(format!("{} (error {})", server.message, server.code))
        }
        mysql_async::Error::Io(_) | mysql_async::Error::Driver(_) => {
            DbError::connection(e.to_string())
        }
        _ => DbError::other(e.to_string()),
    }
}

fn params(params: &[Param]) -> Params {
    if params.is_empty() {
        return Params::Empty;
    }
    Params::Positional(
        params
            .iter()
            .map(|param| match param {
                Param::Null => Value::NULL,
                Param::Text(text) => Value::Bytes(text.clone().into_bytes()),
                Param::Bytes(bytes) => Value::Bytes(bytes.clone()),
            })
            .collect(),
    )
}

/// Converts a column value; `DECIMAL` values arrive as exact text.
pub(crate) fn cell(value: &Value, column_type: ColumnType, charset: u16) -> Cell {
    match value {
        Value::NULL => Cell::Null,
        Value::Int(n) => Cell::Int(*n),
        Value::UInt(n) => Cell::UInt(*n),
        Value::Float(x) => Cell::Float(f64::from(*x)),
        Value::Double(x) => Cell::Float(*x),
        Value::Bytes(bytes) => match column_type {
            ColumnType::MYSQL_TYPE_DECIMAL | ColumnType::MYSQL_TYPE_NEWDECIMAL => {
                Cell::Decimal(String::from_utf8_lossy(bytes).into_owned())
            }
            ColumnType::MYSQL_TYPE_JSON => serde_json::from_slice(bytes).map_or_else(
                |_| Cell::Text(String::from_utf8_lossy(bytes).into_owned()),
                Cell::Json,
            ),
            ColumnType::MYSQL_TYPE_BIT | ColumnType::MYSQL_TYPE_GEOMETRY => {
                Cell::Bytes(bytes.clone())
            }
            _ if charset == BINARY_CHARSET => Cell::Bytes(bytes.clone()),
            _ => Cell::Text(String::from_utf8_lossy(bytes).into_owned()),
        },
        Value::Date(year, month, day, hour, minute, second, micros) => {
            let date = format!("{year:04}-{month:02}-{day:02}");
            if column_type == ColumnType::MYSQL_TYPE_DATE {
                Cell::Date(date)
            } else {
                let seconds =
                    u64::from(*hour) * 3600 + u64::from(*minute) * 60 + u64::from(*second);
                Cell::DateTime(format!(
                    "{date}T{}",
                    time_text(seconds * 1_000_000 + u64::from(*micros), 6)
                ))
            }
        }
        Value::Time(negative, days, hours, minutes, seconds, micros) => {
            let total_hours = u64::from(*days) * 24 + u64::from(*hours);
            let mut text = format!("{total_hours:02}:{minutes:02}:{seconds:02}");
            if *micros > 0 {
                let fraction = format!("{micros:06}");
                text.push('.');
                text.push_str(fraction.trim_end_matches('0'));
            }
            Cell::Time(if *negative { format!("-{text}") } else { text })
        }
    }
}

fn row(row: &mysql_async::Row) -> Row {
    let columns = row
        .columns_ref()
        .iter()
        .enumerate()
        .map(|(index, column)| {
            let value = row.as_ref(index).unwrap_or(&Value::NULL);
            (
                column.name_str().into_owned(),
                cell(value, column.column_type(), column.character_set()),
            )
        })
        .collect();
    Row { columns }
}

#[async_trait]
impl Database for Mysql {
    type Conn = Conn;

    fn style(&self) -> Placeholder {
        Placeholder::Question
    }

    fn describe(&self) -> String {
        let s = &self.settings;
        format!("mysql://{}@{}:{}/{}", s.user, s.host, s.port, s.database)
    }

    async fn connect(&self) -> Result<Conn, DbError> {
        let s = &self.settings;
        let password = self.secret.resolve().map_err(DbError::connection)?;
        let opts = OptsBuilder::default()
            .ip_or_hostname(s.host.clone())
            .tcp_port(s.port)
            .user(Some(s.user.clone()))
            .pass(password)
            .db_name(Some(s.database.clone()))
            .prefer_socket(false)
            .ssl_opts(self.ssl.clone());
        tokio::time::timeout(s.connect_timeout.0, Conn::new(opts))
            .await
            .map_err(|_| DbError::connection("connecting timed out"))?
            .map_err(|e| DbError::connection(e.to_string()))
    }

    async fn fetch(
        &self,
        conn: &mut Conn,
        statement: &Prepared,
        values: &[Param],
        max_rows: usize,
    ) -> Result<Vec<Row>, DbError> {
        let mut result = conn
            .exec_iter(statement.sql.as_str(), params(values))
            .await
            .map_err(|e| classify(&e))?;
        let mut rows = Vec::new();
        while rows.len() < max_rows {
            match result.next().await.map_err(|e| classify(&e))? {
                Some(found) => rows.push(row(&found)),
                None => break,
            }
        }
        result.drop_result().await.map_err(|e| classify(&e))?;
        Ok(rows)
    }

    async fn write(
        &self,
        conn: &mut Conn,
        statements: &[(&Prepared, Vec<Param>)],
    ) -> Result<u64, DbError> {
        let mut tx = conn
            .start_transaction(TxOpts::default())
            .await
            .map_err(|e| classify(&e))?;
        let mut affected = 0;
        for (statement, values) in statements {
            tx.exec_drop(statement.sql.as_str(), params(values))
                .await
                .map_err(|e| classify(&e))?;
            affected += tx.affected_rows();
        }
        tx.commit().await.map_err(|e| classify(&e))?;
        Ok(affected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_values() {
        let text = ColumnType::MYSQL_TYPE_VAR_STRING;
        assert_eq!(
            cell(
                &Value::Bytes(b"5.40".to_vec()),
                ColumnType::MYSQL_TYPE_NEWDECIMAL,
                33
            ),
            Cell::Decimal("5.40".into())
        );
        assert_eq!(
            cell(&Value::Bytes(b"DOE".to_vec()), text, 33),
            Cell::Text("DOE".into())
        );
        assert_eq!(
            cell(
                &Value::Bytes(vec![0, 255]),
                ColumnType::MYSQL_TYPE_BLOB,
                BINARY_CHARSET
            ),
            Cell::Bytes(vec![0, 255])
        );
        assert_eq!(
            cell(
                &Value::Date(2026, 9, 29, 0, 0, 0, 0),
                ColumnType::MYSQL_TYPE_DATE,
                63
            ),
            Cell::Date("2026-09-29".into())
        );
        assert_eq!(
            cell(
                &Value::Date(2026, 9, 29, 12, 30, 5, 250_000),
                ColumnType::MYSQL_TYPE_DATETIME,
                63
            ),
            Cell::DateTime("2026-09-29T12:30:05.25".into())
        );
        assert_eq!(
            cell(
                &Value::Time(true, 1, 2, 3, 4, 0),
                ColumnType::MYSQL_TYPE_TIME,
                63
            ),
            Cell::Time("-26:03:04".into())
        );
        assert_eq!(
            cell(
                &Value::Bytes(br#"{"a":1}"#.to_vec()),
                ColumnType::MYSQL_TYPE_JSON,
                63
            ),
            Cell::Json(serde_json::json!({"a": 1}))
        );
        assert_eq!(cell(&Value::NULL, text, 33), Cell::Null);
        assert_eq!(
            cell(&Value::UInt(7), ColumnType::MYSQL_TYPE_LONGLONG, 63),
            Cell::UInt(7)
        );
    }

    #[test]
    fn binds_text_and_null() {
        let Params::Positional(values) =
            params(&[Param::Text("P1".into()), Param::Null, Param::Bytes(vec![1])])
        else {
            panic!("positional parameters expected");
        };
        assert_eq!(
            values,
            [
                Value::Bytes(b"P1".to_vec()),
                Value::NULL,
                Value::Bytes(vec![1])
            ]
        );
        assert!(matches!(params(&[]), Params::Empty));
    }
}
