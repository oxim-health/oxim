//! Microsoft SQL Server.
//!
//! Encrypted connections use TDS 8.0 strict encryption (SQL Server 2022 and
//! Azure SQL): OXIM opens TLS with rustls before the TDS login, so the
//! server certificate is always verified. Older servers that only offer
//! TLS inside the TDS login cannot be reached encrypted; `encryption: none`
//! connects to them in the clear, for isolated networks only.

use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;

use oxim_connectors::tls::{ClientTlsSettings, Stream, client_config, server_name};
use oxim_core::config::DurationText;
use oxim_core::{EngineError, Settings, async_trait};
use rustls_pki_types::ServerName;
use serde::Deserialize;
use tiberius::{AuthMethod, Client, ColumnData, EncryptionLevel, ToSql};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};
use tracing::warn;

use crate::connector::{Database, DbError};
use crate::settings::{Secret, parse};
use crate::sql::{Placeholder, Prepared};
use crate::value::{Cell, Param, Row, date_text, offset_text, scaled_decimal, time_text};

fn default_port() -> u16 {
    1433
}

fn default_connect_timeout() -> DurationText {
    DurationText(Duration::from_secs(10))
}

/// How the connection is protected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Encryption {
    /// TDS 8.0 strict: TLS before the login, certificate verified.
    #[default]
    Strict,
    /// No encryption at all, including the login.
    None,
}

/// Connection settings of `mssql`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MssqlSettings {
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
    encryption: Encryption,
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
    "encryption",
    "tls",
    "connect_timeout",
];

/// A SQL Server instance.
#[derive(Clone)]
pub(crate) struct Mssql {
    settings: MssqlSettings,
    secret: Secret,
    tls: Option<(TlsConnector, ServerName<'static>)>,
}

impl std::fmt::Debug for Mssql {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.describe())
    }
}

/// The ALPN protocol of TDS 8.0.
const TDS_8_ALPN: &[u8] = b"tds/8.0";

impl Mssql {
    pub(crate) fn from_settings(settings: Settings) -> Result<Self, EngineError> {
        let settings: MssqlSettings = parse(settings, "mssql connection")?;
        let secret = Secret::new(settings.password.clone(), settings.password_env.clone())?;
        let tls = match settings.encryption {
            Encryption::Strict => {
                let tls = match &settings.tls {
                    Some(tls) => tls.clone(),
                    None => serde_json::from_value(serde_json::json!({}))
                        .map_err(|e| EngineError::Config(e.to_string()))?,
                };
                let mut config = client_config(&tls)?;
                config.alpn_protocols = vec![TDS_8_ALPN.to_vec()];
                Some((
                    TlsConnector::from(Arc::new(config)),
                    server_name(&tls, &settings.host)?,
                ))
            }
            Encryption::None => {
                if settings.tls.is_some() {
                    return Err(EngineError::Config(
                        "tls settings need encryption: strict".into(),
                    ));
                }
                None
            }
        };
        Ok(Self {
            settings,
            secret,
            tls,
        })
    }
}

/// SQL Server errors for rejected data: duplicate keys, foreign keys and
/// check constraints, `NOT NULL`, truncation, conversion and overflow.
const REJECTED_CODES: &[u32] = &[
    220, 241, 242, 244, 245, 248, 515, 547, 2601, 2627, 2628, 8114, 8115, 8152,
];

fn classify(e: &tiberius::error::Error) -> DbError {
    use tiberius::error::Error;
    match e {
        Error::Server(token) if REJECTED_CODES.contains(&token.code()) => {
            DbError::rejected(format!("{} (error {})", token.message(), token.code()))
        }
        Error::Server(token) => {
            DbError::other(format!("{} (error {})", token.message(), token.code()))
        }
        Error::Io { .. } | Error::Tls(_) | Error::Routing { .. } | Error::Protocol(_) => {
            DbError::connection(e.to_string())
        }
        _ => DbError::other(e.to_string()),
    }
}

/// A parameter: text is sent as `NVARCHAR` and converted by the server.
#[derive(Debug)]
struct TdsParam<'a>(&'a Param);

impl ToSql for TdsParam<'_> {
    fn to_sql(&self) -> ColumnData<'_> {
        match self.0 {
            Param::Null => ColumnData::String(None),
            Param::Text(text) => ColumnData::String(Some(Cow::Borrowed(text))),
            Param::Bytes(bytes) => ColumnData::Binary(Some(Cow::Borrowed(bytes))),
        }
    }
}

/// Days from 0001-01-01 and from 1900-01-01 to 1970-01-01.
const DAYS_FROM_YEAR_ONE: i64 = 719_162;
const DAYS_FROM_1900: i64 = 25_567;

/// The date and time text of a day count (since 1970-01-01) and a time of
/// day in units of `10^-scale` seconds.
fn date_time_text(unix_days: i64, ticks: u64, scale: u32) -> String {
    format!("{}T{}", date_text(unix_days), time_text(ticks, scale))
}

/// Converts a column value; exact numerics stay exact text.
pub(crate) fn cell(data: &ColumnData<'_>) -> Cell {
    match data {
        ColumnData::U8(value) => value.map_or(Cell::Null, |v| Cell::Int(v.into())),
        ColumnData::I16(value) => value.map_or(Cell::Null, |v| Cell::Int(v.into())),
        ColumnData::I32(value) => value.map_or(Cell::Null, |v| Cell::Int(v.into())),
        ColumnData::I64(value) => value.map_or(Cell::Null, Cell::Int),
        ColumnData::F32(value) => value.map_or(Cell::Null, |v| Cell::Float(v.into())),
        ColumnData::F64(value) => value.map_or(Cell::Null, Cell::Float),
        ColumnData::Bit(value) => value.map_or(Cell::Null, Cell::Bool),
        ColumnData::String(value) => value
            .as_ref()
            .map_or(Cell::Null, |v| Cell::Text(v.to_string())),
        ColumnData::Guid(value) => value.map_or(Cell::Null, |v| Cell::Text(v.to_string())),
        ColumnData::Binary(value) => value
            .as_ref()
            .map_or(Cell::Null, |v| Cell::Bytes(v.to_vec())),
        ColumnData::Numeric(value) => value.map_or(Cell::Null, |n| {
            Cell::Decimal(scaled_decimal(n.value(), n.scale().into()))
        }),
        ColumnData::Xml(value) => value
            .as_ref()
            .map_or(Cell::Null, |v| Cell::Text(v.to_string())),
        ColumnData::DateTime(value) => value.map_or(Cell::Null, |v| {
            // 1/300 second units, rounded to milliseconds as SQL Server does.
            let millis = (u64::from(v.seconds_fragments()) * 10 + 1) / 3;
            Cell::DateTime(date_time_text(
                i64::from(v.days()) - DAYS_FROM_1900,
                millis,
                3,
            ))
        }),
        ColumnData::SmallDateTime(value) => value.map_or(Cell::Null, |v| {
            // `smalldatetime` counts minutes since midnight.
            Cell::DateTime(date_time_text(
                i64::from(v.days()) - DAYS_FROM_1900,
                u64::from(v.seconds_fragments()) * 60,
                0,
            ))
        }),
        ColumnData::Time(value) => value.map_or(Cell::Null, |v| {
            Cell::Time(time_text(v.increments(), v.scale().into()))
        }),
        ColumnData::Date(value) => value.map_or(Cell::Null, |v| {
            Cell::Date(date_text(i64::from(v.days()) - DAYS_FROM_YEAR_ONE))
        }),
        ColumnData::DateTime2(value) => value.map_or(Cell::Null, |v| {
            Cell::DateTime(date_time_text(
                i64::from(v.date().days()) - DAYS_FROM_YEAR_ONE,
                v.time().increments(),
                v.time().scale().into(),
            ))
        }),
        ColumnData::DateTimeOffset(value) => value.map_or(Cell::Null, |v| {
            // The stored time is UTC; show the local time with its offset.
            let dt = v.datetime2();
            let scale = u32::from(dt.time().scale());
            let per_second = 10i128.pow(scale);
            let per_day = 86_400 * per_second;
            let utc = i128::from(i64::from(dt.date().days()) - DAYS_FROM_YEAR_ONE) * per_day
                + i128::from(dt.time().increments());
            let local = utc + i128::from(v.offset()) * 60 * per_second;
            let days = i64::try_from(local.div_euclid(per_day)).unwrap_or(0);
            let ticks = u64::try_from(local.rem_euclid(per_day)).unwrap_or(0);
            Cell::DateTime(format!(
                "{}{}",
                date_time_text(days, ticks, scale),
                offset_text(i32::from(v.offset()))
            ))
        }),
    }
}

fn row(row: &tiberius::Row) -> Row {
    Row {
        columns: row
            .cells()
            .map(|(column, data)| (column.name().to_owned(), cell(data)))
            .collect(),
    }
}

type Conn = Client<Compat<Stream>>;

#[async_trait]
impl Database for Mssql {
    type Conn = Conn;

    fn style(&self) -> Placeholder {
        Placeholder::AtP
    }

    fn describe(&self) -> String {
        let s = &self.settings;
        format!("mssql://{}@{}:{}/{}", s.user, s.host, s.port, s.database)
    }

    async fn connect(&self) -> Result<Conn, DbError> {
        let s = &self.settings;
        let password = self.secret.resolve().map_err(DbError::connection)?;
        let open = async {
            let tcp = TcpStream::connect((s.host.as_str(), s.port))
                .await
                .map_err(|e| DbError::connection(format!("cannot connect: {e}")))?;
            let _ = tcp.set_nodelay(true);
            let stream = match &self.tls {
                Some((connector, name)) => Stream::Client(Box::new(
                    connector
                        .connect(name.clone(), tcp)
                        .await
                        .map_err(|e| DbError::connection(format!("TLS handshake failed: {e}")))?,
                )),
                None => {
                    warn!(database = %self.describe(), "connecting without encryption");
                    Stream::Plain(tcp)
                }
            };
            let mut config = tiberius::Config::new();
            config.host(&s.host);
            config.port(s.port);
            config.database(&s.database);
            config.application_name("oxim");
            // TLS, if any, is already established below the TDS layer.
            config.encryption(EncryptionLevel::NotSupported);
            config.authentication(AuthMethod::sql_server(
                &s.user,
                password.unwrap_or_default(),
            ));
            Client::connect(config, stream.compat_write())
                .await
                .map_err(|e| classify(&e))
        };
        tokio::time::timeout(s.connect_timeout.0, open)
            .await
            .map_err(|_| DbError::connection("connecting timed out"))?
    }

    async fn fetch(
        &self,
        conn: &mut Conn,
        statement: &Prepared,
        params: &[Param],
        max_rows: usize,
    ) -> Result<Vec<Row>, DbError> {
        let wrapped: Vec<TdsParam<'_>> = params.iter().map(TdsParam).collect();
        let refs: Vec<&dyn ToSql> = wrapped.iter().map(|p| p as &dyn ToSql).collect();
        let stream = conn
            .query(statement.sql.as_str(), &refs)
            .await
            .map_err(|e| classify(&e))?;
        // The whole result is read so the connection stays in step; limit
        // large tables in the query (TOP) as well.
        let rows = stream.into_first_result().await.map_err(|e| classify(&e))?;
        Ok(rows.iter().take(max_rows).map(row).collect())
    }

    async fn write(
        &self,
        conn: &mut Conn,
        statements: &[(&Prepared, Vec<Param>)],
    ) -> Result<u64, DbError> {
        conn.begin_transaction().await.map_err(|e| classify(&e))?;
        let mut affected = 0;
        for (statement, params) in statements {
            let wrapped: Vec<TdsParam<'_>> = params.iter().map(TdsParam).collect();
            let refs: Vec<&dyn ToSql> = wrapped.iter().map(|p| p as &dyn ToSql).collect();
            match conn.execute(statement.sql.as_str(), &refs).await {
                Ok(result) => affected += result.total(),
                Err(e) => {
                    let error = classify(&e);
                    if error.kind != crate::connector::ErrorKind::Connection {
                        let _ = conn.rollback_transaction().await;
                    }
                    return Err(error);
                }
            }
        }
        conn.commit_transaction().await.map_err(|e| classify(&e))?;
        Ok(affected)
    }
}

#[cfg(test)]
mod tests {
    use tiberius::numeric::Numeric;
    use tiberius::time::{Date, DateTime, DateTime2, DateTimeOffset, SmallDateTime, Time};

    use super::*;

    #[test]
    fn converts_values() {
        assert_eq!(
            cell(&ColumnData::Numeric(Some(Numeric::new_with_scale(540, 2)))),
            Cell::Decimal("5.40".into())
        );
        assert_eq!(cell(&ColumnData::I32(None)), Cell::Null);
        assert_eq!(
            cell(&ColumnData::String(Some("DOE".into()))),
            Cell::Text("DOE".into())
        );
        // 2026-09-29 is day 739887 counted from 0001-01-01.
        let date = Date::new(739_887);
        assert_eq!(
            cell(&ColumnData::Date(Some(date))),
            Cell::Date("2026-09-29".into())
        );
        let time = Time::new(451_250_000_000, 7);
        assert_eq!(
            cell(&ColumnData::Time(Some(time))),
            Cell::Time("12:32:05".into())
        );
        assert_eq!(
            cell(&ColumnData::DateTime2(Some(DateTime2::new(date, time)))),
            Cell::DateTime("2026-09-29T12:32:05".into())
        );
        // 09:32:05 UTC at +03:00 is 12:32:05 local time.
        let utc = DateTime2::new(date, Time::new(343_250_000_000, 7));
        assert_eq!(
            cell(&ColumnData::DateTimeOffset(Some(DateTimeOffset::new(
                utc, 180
            )))),
            Cell::DateTime("2026-09-29T12:32:05+03:00".into())
        );
        // datetime: 2026-09-29 is day 46292 since 1900-01-01; 150 units = 0.5 s.
        assert_eq!(
            cell(&ColumnData::DateTime(Some(DateTime::new(46_292, 150)))),
            Cell::DateTime("2026-09-29T00:00:00.5".into())
        );
        assert_eq!(
            cell(&ColumnData::SmallDateTime(Some(SmallDateTime::new(
                46_292, 90
            )))),
            Cell::DateTime("2026-09-29T01:30:00".into())
        );
    }

    #[test]
    fn encryption_settings() {
        let settings = |extra: serde_json::Value| {
            let mut json =
                serde_json::json!({"host": "db.example.org", "database": "lis", "user": "oxim"});
            if let (Some(base), Some(extra)) = (json.as_object_mut(), extra.as_object()) {
                base.extend(extra.clone());
            }
            match json {
                serde_json::Value::Object(map) => map,
                _ => Settings::new(),
            }
        };
        assert!(Mssql::from_settings(settings(serde_json::json!({"encryption": "none"}))).is_ok());
        assert!(
            Mssql::from_settings(settings(
                serde_json::json!({"encryption": "none", "tls": {}})
            ))
            .is_err()
        );
        assert!(
            Mssql::from_settings(settings(
                serde_json::json!({"tls": {"system_roots": false}})
            ))
            .is_err()
        );
    }
}
