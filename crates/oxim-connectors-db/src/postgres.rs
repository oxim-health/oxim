//! PostgreSQL.

use std::time::Duration;

use bytes::BytesMut;
use futures_util::StreamExt;
use oxim_connectors::tls::{ClientTlsSettings, client_config};
use oxim_core::config::DurationText;
use oxim_core::{EngineError, Settings, async_trait};
use serde::Deserialize;
use tokio_postgres::config::SslMode;
use tokio_postgres::types::{Format, FromSql, IsNull, Kind, ToSql, Type, to_sql_checked};
use tokio_postgres::{Client, NoTls};
use tokio_postgres_rustls::MakeRustlsConnect;
use tracing::debug;

use crate::connector::{Database, DbError};
use crate::settings::{Secret, parse};
use crate::sql::{Placeholder, Prepared};
use crate::value::{Cell, Param, Row, date_text, offset_text, scaled_decimal, time_text};

fn default_port() -> u16 {
    5432
}

fn default_connect_timeout() -> DurationText {
    DurationText(Duration::from_secs(10))
}

fn default_application_name() -> String {
    "oxim".into()
}

/// Connection settings of `postgres`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PostgresSettings {
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
    #[serde(default = "default_application_name")]
    application_name: String,
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
    "application_name",
];

/// A PostgreSQL server.
#[derive(Clone)]
pub(crate) struct Postgres {
    settings: PostgresSettings,
    secret: Secret,
    tls: Option<MakeRustlsConnect>,
}

impl std::fmt::Debug for Postgres {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.describe())
    }
}

impl Postgres {
    pub(crate) fn from_settings(settings: Settings) -> Result<Self, EngineError> {
        let settings: PostgresSettings = parse(settings, "postgres connection")?;
        let secret = Secret::new(settings.password.clone(), settings.password_env.clone())?;
        let tls = match &settings.tls {
            Some(tls) => {
                if tls.server_name.is_some() {
                    return Err(EngineError::Config(
                        "postgres verifies the certificate against host; server_name is not supported"
                            .into(),
                    ));
                }
                Some(MakeRustlsConnect::new(client_config(tls)?))
            }
            None => None,
        };
        Ok(Self {
            settings,
            secret,
            tls,
        })
    }
}

/// Classifies an error by its SQLSTATE class.
fn classify(e: &tokio_postgres::Error) -> DbError {
    let message = match e.as_db_error() {
        Some(db) => format!("{} (SQLSTATE {})", db.message(), db.code().code()),
        None => e.to_string(),
    };
    if e.is_closed() {
        return DbError::connection(message);
    }
    match e.code().map(|code| code.code()) {
        Some(code) if code.starts_with("22") || code.starts_with("23") => {
            DbError::rejected(message)
        }
        Some(code) if code.starts_with("08") || code.starts_with("57P") => {
            DbError::connection(message)
        }
        Some(_) => DbError::other(message),
        None => {
            let io = std::error::Error::source(e).is_some_and(|s| s.is::<std::io::Error>());
            if io {
                DbError::connection(message)
            } else {
                DbError::other(message)
            }
        }
    }
}

/// A parameter sent in text format, which the server converts to the
/// parameter's type; binary data for `bytea` is sent as is.
#[derive(Debug)]
struct PgParam<'a>(&'a Param);

impl ToSql for PgParam<'_> {
    fn to_sql(
        &self,
        ty: &Type,
        out: &mut BytesMut,
    ) -> Result<IsNull, Box<dyn std::error::Error + Sync + Send>> {
        match self.0 {
            Param::Null => return Ok(IsNull::Yes),
            Param::Text(text) => out.extend_from_slice(text.as_bytes()),
            Param::Bytes(bytes) if *ty == Type::BYTEA => out.extend_from_slice(bytes),
            Param::Bytes(bytes) => out.extend_from_slice(String::from_utf8_lossy(bytes).as_bytes()),
        }
        Ok(IsNull::No)
    }

    fn accepts(_: &Type) -> bool {
        true
    }

    fn encode_format(&self, ty: &Type) -> Format {
        match self.0 {
            Param::Bytes(_) if *ty == Type::BYTEA => Format::Binary,
            _ => Format::Text,
        }
    }

    to_sql_checked!();
}

/// A column value in PostgreSQL's binary format.
struct Raw<'a>(&'a [u8]);

impl<'a> FromSql<'a> for Raw<'a> {
    fn from_sql(_: &Type, raw: &'a [u8]) -> Result<Self, Box<dyn std::error::Error + Sync + Send>> {
        Ok(Raw(raw))
    }

    fn accepts(_: &Type) -> bool {
        true
    }
}

/// Days from 1970-01-01 to PostgreSQL's epoch, 2000-01-01.
const PG_EPOCH_DAYS: i64 = 10_957;
const MICROS_PER_DAY: i64 = 86_400_000_000;

fn be<const N: usize>(raw: &[u8], at: usize) -> Option<[u8; N]> {
    raw.get(at..at + N).and_then(|slice| slice.try_into().ok())
}

/// The exact decimal text of a binary `numeric`.
fn numeric_text(raw: &[u8]) -> Option<String> {
    let ndigits = usize::try_from(i16::from_be_bytes(be(raw, 0)?)).ok()?;
    let weight = i32::from(i16::from_be_bytes(be(raw, 2)?));
    let sign = u16::from_be_bytes(be(raw, 4)?);
    let dscale = usize::from(u16::from_be_bytes(be(raw, 6)?));
    let digits: Vec<i32> = (0..ndigits)
        .map(|i| be(raw, 8 + 2 * i).map(|b| i32::from(i16::from_be_bytes(b))))
        .collect::<Option<_>>()?;
    match sign {
        0x0000 | 0x4000 => {}
        0xC000 => return Some("NaN".into()),
        0xD000 => return Some("Infinity".into()),
        0xF000 => return Some("-Infinity".into()),
        _ => return None,
    }
    let digit = |i: i32| {
        usize::try_from(i)
            .ok()
            .and_then(|i| digits.get(i).copied())
            .unwrap_or(0)
    };
    let mut text = String::new();
    if weight >= 0 {
        for i in 0..=weight {
            if text.is_empty() {
                text.push_str(&digit(i).to_string());
            } else {
                text.push_str(&format!("{:04}", digit(i)));
            }
        }
    } else {
        text.push('0');
    }
    if dscale > 0 {
        let mut fraction = String::new();
        let mut k = 1;
        while fraction.len() < dscale {
            fraction.push_str(&format!("{:04}", digit(weight + k)));
            k += 1;
        }
        fraction.truncate(dscale);
        text.push('.');
        text.push_str(&fraction);
    }
    if sign == 0x4000 {
        text.insert(0, '-');
    }
    Some(text)
}

/// `YYYY-MM-DDTHH:MM:SS[.ffffff]` for microseconds since 2000-01-01.
fn timestamp_text(micros: i64) -> String {
    match micros {
        i64::MAX => "infinity".into(),
        i64::MIN => "-infinity".into(),
        _ => {
            let days = micros.div_euclid(MICROS_PER_DAY);
            let time = micros.rem_euclid(MICROS_PER_DAY);
            format!(
                "{}T{}",
                date_text(days + PG_EPOCH_DAYS),
                time_text(u64::try_from(time).unwrap_or(0), 6)
            )
        }
    }
}

fn uuid_text(raw: &[u8]) -> Option<String> {
    let hex: String = raw.get(..16)?.iter().map(|b| format!("{b:02x}")).collect();
    Some(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}

fn text(raw: &[u8]) -> String {
    String::from_utf8_lossy(raw).into_owned()
}

/// Converts a binary column value. Types without a conversion (arrays,
/// network addresses, geometry) become Base64; cast them to `text` in the
/// query for a readable value.
fn decode(ty: &Type, raw: &[u8]) -> Cell {
    let decoded = match *ty {
        Type::BOOL => raw.first().map(|b| Cell::Bool(*b != 0)),
        Type::INT2 => be(raw, 0).map(|b| Cell::Int(i16::from_be_bytes(b).into())),
        Type::INT4 => be(raw, 0).map(|b| Cell::Int(i32::from_be_bytes(b).into())),
        Type::INT8 => be(raw, 0).map(|b| Cell::Int(i64::from_be_bytes(b))),
        Type::OID => be(raw, 0).map(|b| Cell::UInt(u32::from_be_bytes(b).into())),
        Type::FLOAT4 => be(raw, 0).map(|b| Cell::Float(f32::from_be_bytes(b).into())),
        Type::FLOAT8 => be(raw, 0).map(|b| Cell::Float(f64::from_be_bytes(b))),
        Type::NUMERIC => numeric_text(raw).map(Cell::Decimal),
        Type::TEXT
        | Type::VARCHAR
        | Type::BPCHAR
        | Type::NAME
        | Type::UNKNOWN
        | Type::XML
        | Type::CHAR => Some(Cell::Text(text(raw))),
        Type::BYTEA => Some(Cell::Bytes(raw.to_vec())),
        Type::DATE => be(raw, 0).map(|b| match i32::from_be_bytes(b) {
            i32::MAX => Cell::Date("infinity".into()),
            i32::MIN => Cell::Date("-infinity".into()),
            days => Cell::Date(date_text(i64::from(days) + PG_EPOCH_DAYS)),
        }),
        Type::TIME => be(raw, 0).map(|b| {
            Cell::Time(time_text(
                u64::try_from(i64::from_be_bytes(b)).unwrap_or(0),
                6,
            ))
        }),
        Type::TIMETZ => be(raw, 0).zip(be(raw, 8)).map(|(time, zone)| {
            // The zone is stored in seconds west of UTC.
            let offset = -i32::from_be_bytes(zone) / 60;
            Cell::Time(format!(
                "{}{}",
                time_text(u64::try_from(i64::from_be_bytes(time)).unwrap_or(0), 6),
                offset_text(offset)
            ))
        }),
        Type::TIMESTAMP => {
            be(raw, 0).map(|b| Cell::DateTime(timestamp_text(i64::from_be_bytes(b))))
        }
        Type::TIMESTAMPTZ => be(raw, 0).map(|b| match i64::from_be_bytes(b) {
            micros @ (i64::MAX | i64::MIN) => Cell::DateTime(timestamp_text(micros)),
            micros => Cell::DateTime(format!("{}Z", timestamp_text(micros))),
        }),
        Type::INTERVAL => {
            be(raw, 0)
                .zip(be(raw, 8))
                .zip(be(raw, 12))
                .map(|((micros, days), months)| {
                    let micros = i64::from_be_bytes(micros);
                    let seconds = scaled_decimal(i128::from(micros), 6);
                    let seconds = seconds.trim_end_matches('0').trim_end_matches('.');
                    Cell::Text(format!(
                        "P{}M{}DT{}S",
                        i32::from_be_bytes(months),
                        i32::from_be_bytes(days),
                        seconds
                    ))
                })
        }
        Type::UUID => uuid_text(raw).map(Cell::Text),
        Type::JSON => {
            Some(serde_json::from_slice(raw).map_or_else(|_| Cell::Text(text(raw)), Cell::Json))
        }
        Type::JSONB => raw.split_first().map(|(_, body)| {
            serde_json::from_slice(body).map_or_else(|_| Cell::Text(text(body)), Cell::Json)
        }),
        _ => match ty.kind() {
            Kind::Enum(_) => Some(Cell::Text(text(raw))),
            Kind::Domain(inner) => Some(decode(inner, raw)),
            _ if ty.name() == "citext" => Some(Cell::Text(text(raw))),
            _ => None,
        },
    };
    decoded.unwrap_or_else(|| Cell::Bytes(raw.to_vec()))
}

fn row(row: &tokio_postgres::Row) -> Result<Row, DbError> {
    let mut columns = Vec::with_capacity(row.len());
    for (index, column) in row.columns().iter().enumerate() {
        let value: Option<Raw<'_>> = row
            .try_get(index)
            .map_err(|e| DbError::other(format!("column {}: {e}", column.name())))?;
        let cell = value.map_or(Cell::Null, |raw| decode(column.type_(), raw.0));
        columns.push((column.name().to_owned(), cell));
    }
    Ok(Row { columns })
}

#[async_trait]
impl Database for Postgres {
    type Conn = Client;

    fn style(&self) -> Placeholder {
        Placeholder::Dollar
    }

    fn describe(&self) -> String {
        let s = &self.settings;
        format!("postgres://{}@{}:{}/{}", s.user, s.host, s.port, s.database)
    }

    async fn connect(&self) -> Result<Client, DbError> {
        let s = &self.settings;
        let mut config = tokio_postgres::Config::new();
        config
            .host(&s.host)
            .port(s.port)
            .dbname(&s.database)
            .user(&s.user)
            .application_name(&s.application_name)
            .connect_timeout(s.connect_timeout.0);
        if let Some(password) = self.secret.resolve().map_err(DbError::connection)? {
            config.password(password);
        }
        let describe = self.describe();
        let client = match &self.tls {
            Some(tls) => {
                config.ssl_mode(SslMode::Require);
                let (client, connection) = config
                    .connect(tls.clone())
                    .await
                    .map_err(|e| DbError::connection(e.to_string()))?;
                tokio::spawn(async move {
                    if let Err(error) = connection.await {
                        debug!(database = %describe, %error, "connection closed");
                    }
                });
                client
            }
            None => {
                config.ssl_mode(SslMode::Disable);
                let (client, connection) = config
                    .connect(NoTls)
                    .await
                    .map_err(|e| DbError::connection(e.to_string()))?;
                tokio::spawn(async move {
                    if let Err(error) = connection.await {
                        debug!(database = %describe, %error, "connection closed");
                    }
                });
                client
            }
        };
        Ok(client)
    }

    async fn fetch(
        &self,
        conn: &mut Client,
        statement: &Prepared,
        params: &[Param],
        max_rows: usize,
    ) -> Result<Vec<Row>, DbError> {
        if conn.is_closed() {
            return Err(DbError::connection("the connection is closed"));
        }
        let prepared = conn
            .prepare(&statement.sql)
            .await
            .map_err(|e| classify(&e))?;
        let wrapped: Vec<PgParam<'_>> = params.iter().map(PgParam).collect();
        let stream = conn
            .query_raw(&prepared, wrapped.iter().map(|p| p as &(dyn ToSql + Sync)))
            .await
            .map_err(|e| classify(&e))?;
        let mut stream = std::pin::pin!(stream);
        let mut rows = Vec::new();
        while rows.len() < max_rows {
            match stream.next().await {
                Some(Ok(found)) => rows.push(row(&found)?),
                Some(Err(e)) => return Err(classify(&e)),
                None => break,
            }
        }
        Ok(rows)
    }

    async fn write(
        &self,
        conn: &mut Client,
        statements: &[(&Prepared, Vec<Param>)],
    ) -> Result<u64, DbError> {
        if conn.is_closed() {
            return Err(DbError::connection("the connection is closed"));
        }
        let tx = conn.transaction().await.map_err(|e| classify(&e))?;
        let mut affected = 0;
        for (statement, params) in statements {
            let wrapped: Vec<PgParam<'_>> = params.iter().map(PgParam).collect();
            let refs: Vec<&(dyn ToSql + Sync)> =
                wrapped.iter().map(|p| p as &(dyn ToSql + Sync)).collect();
            affected += tx
                .execute(statement.sql.as_str(), &refs)
                .await
                .map_err(|e| classify(&e))?;
        }
        tx.commit().await.map_err(|e| classify(&e))?;
        Ok(affected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numeric(ndigits: i16, weight: i16, sign: u16, dscale: u16, digits: &[i16]) -> Vec<u8> {
        let mut raw = Vec::new();
        raw.extend_from_slice(&ndigits.to_be_bytes());
        raw.extend_from_slice(&weight.to_be_bytes());
        raw.extend_from_slice(&sign.to_be_bytes());
        raw.extend_from_slice(&dscale.to_be_bytes());
        for d in digits {
            raw.extend_from_slice(&d.to_be_bytes());
        }
        raw
    }

    #[test]
    fn decodes_numeric_exactly() {
        assert_eq!(
            numeric_text(&numeric(2, 0, 0, 2, &[5, 4000])).unwrap(),
            "5.40"
        );
        assert_eq!(
            numeric_text(&numeric(3, 1, 0, 3, &[1, 2345, 6780])).unwrap(),
            "12345.678"
        );
        assert_eq!(
            numeric_text(&numeric(2, -1, 0, 5, &[1, 2000])).unwrap(),
            "0.00012"
        );
        assert_eq!(
            numeric_text(&numeric(1, 1, 0x4000, 0, &[7])).unwrap(),
            "-70000"
        );
        assert_eq!(numeric_text(&numeric(0, 0, 0, 2, &[])).unwrap(), "0.00");
        assert_eq!(numeric_text(&numeric(0, 0, 0xC000, 0, &[])).unwrap(), "NaN");
        assert!(numeric_text(&[0, 1]).is_none());
    }

    #[test]
    fn decodes_temporal_and_other_values() {
        let micros: i64 = 843_307_200_000_000 + 500_000; // 2026-09-21T12:00:00.5
        assert_eq!(
            decode(&Type::TIMESTAMPTZ, &micros.to_be_bytes()),
            Cell::DateTime("2026-09-21T12:00:00.5Z".into())
        );
        assert_eq!(
            decode(&Type::DATE, &9_763i32.to_be_bytes()),
            Cell::Date("2026-09-24".into())
        );
        let mut timetz = 45_000_000_000i64.to_be_bytes().to_vec();
        timetz.extend_from_slice(&(-10_800i32).to_be_bytes());
        assert_eq!(
            decode(&Type::TIMETZ, &timetz),
            Cell::Time("12:30:00+03:00".into())
        );
        assert_eq!(decode(&Type::INT4, &42i32.to_be_bytes()), Cell::Int(42));
        assert_eq!(decode(&Type::BOOL, &[1]), Cell::Bool(true));
        assert_eq!(
            decode(&Type::UUID, &[0x12; 16]),
            Cell::Text("12121212-1212-1212-1212-121212121212".into())
        );
        assert_eq!(
            decode(&Type::JSONB, b"\x01{\"a\":1}"),
            Cell::Json(serde_json::json!({"a": 1}))
        );
        let mut interval = 90_500_000i64.to_be_bytes().to_vec();
        interval.extend_from_slice(&2i32.to_be_bytes());
        interval.extend_from_slice(&1i32.to_be_bytes());
        assert_eq!(
            decode(&Type::INTERVAL, &interval),
            Cell::Text("P1M2DT90.5S".into())
        );
        assert_eq!(decode(&Type::INET, &[1, 2]), Cell::Bytes(vec![1, 2]));
    }

    #[test]
    fn classifies_parameters_in_text_format() {
        let text = Param::Text("42".into());
        let bytes = Param::Bytes(vec![0, 1]);
        assert!(matches!(
            PgParam(&text).encode_format(&Type::INT4),
            Format::Text
        ));
        assert!(matches!(
            PgParam(&bytes).encode_format(&Type::BYTEA),
            Format::Binary
        ));
        let mut out = BytesMut::new();
        assert!(matches!(
            PgParam(&Param::Null).to_sql(&Type::INT4, &mut out),
            Ok(IsNull::Yes)
        ));
        PgParam(&text).to_sql(&Type::INT4, &mut out).unwrap();
        assert_eq!(&out[..], b"42");
    }

    #[test]
    fn rejects_a_server_name_override() {
        let settings = serde_json::json!({
            "host": "db.example.org",
            "database": "lis",
            "user": "oxim",
            "tls": {"server_name": "other", "system_roots": true}
        });
        let serde_json::Value::Object(settings) = settings else {
            unreachable!()
        };
        assert!(Postgres::from_settings(settings).is_err());
    }
}
