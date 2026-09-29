//! SQLite database files.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use oxim_core::config::DurationText;
use oxim_core::{EngineError, Settings, async_trait};
use rusqlite::types::{Value, ValueRef};
use rusqlite::{Connection, ErrorCode, OpenFlags};
use serde::Deserialize;

use crate::connector::{Database, DbError};
use crate::settings::parse;
use crate::sql::{Placeholder, Prepared};
use crate::value::{Cell, Param, Row};

fn default_busy_timeout() -> DurationText {
    DurationText(Duration::from_secs(5))
}

/// Connection settings of `sqlite`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SqliteSettings {
    /// The database file.
    pub(crate) path: PathBuf,
    /// How long to wait for a lock held by another process.
    #[serde(default = "default_busy_timeout")]
    pub(crate) busy_timeout: DurationText,
    /// Whether to create the file when it does not exist.
    #[serde(default)]
    pub(crate) create: bool,
}

/// The connection setting names.
pub(crate) const KEYS: &[&str] = &["path", "busy_timeout", "create"];

/// A SQLite database file.
#[derive(Debug, Clone)]
pub(crate) struct Sqlite {
    settings: SqliteSettings,
}

impl Sqlite {
    pub(crate) fn from_settings(settings: Settings) -> Result<Self, EngineError> {
        Ok(Self {
            settings: parse(settings, "sqlite connection")?,
        })
    }
}

fn error(e: &rusqlite::Error) -> DbError {
    match e.sqlite_error_code() {
        Some(ErrorCode::ConstraintViolation | ErrorCode::TypeMismatch | ErrorCode::TooBig) => {
            DbError::rejected(e.to_string())
        }
        Some(ErrorCode::CannotOpen | ErrorCode::NotADatabase) => DbError::connection(e.to_string()),
        _ => DbError::other(e.to_string()),
    }
}

fn value(param: &Param) -> Value {
    match param {
        Param::Null => Value::Null,
        Param::Text(text) => Value::Text(text.clone()),
        Param::Bytes(bytes) => Value::Blob(bytes.clone()),
    }
}

fn cell(value: ValueRef<'_>) -> Cell {
    match value {
        ValueRef::Null => Cell::Null,
        ValueRef::Integer(n) => Cell::Int(n),
        ValueRef::Real(x) => Cell::Float(x),
        ValueRef::Text(bytes) => Cell::Text(String::from_utf8_lossy(bytes).into_owned()),
        ValueRef::Blob(bytes) => Cell::Bytes(bytes.to_vec()),
    }
}

type Shared = Arc<Mutex<Connection>>;

async fn blocking<T: Send + 'static>(
    conn: &Shared,
    work: impl FnOnce(&mut Connection) -> Result<T, DbError> + Send + 'static,
) -> Result<T, DbError> {
    let conn = conn.clone();
    tokio::task::spawn_blocking(move || {
        let mut guard = conn
            .lock()
            .map_err(|_| DbError::connection("the SQLite connection is unusable"))?;
        work(&mut guard)
    })
    .await
    .map_err(|e| DbError::connection(format!("SQLite task failed: {e}")))?
}

#[async_trait]
impl Database for Sqlite {
    type Conn = Shared;

    fn style(&self) -> Placeholder {
        Placeholder::NumberedQuestion
    }

    fn describe(&self) -> String {
        format!("sqlite:{}", self.settings.path.display())
    }

    async fn connect(&self) -> Result<Shared, DbError> {
        let settings = self.settings.clone();
        tokio::task::spawn_blocking(move || {
            let mut flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
            if settings.create {
                flags |= OpenFlags::SQLITE_OPEN_CREATE;
            }
            let conn = Connection::open_with_flags(&settings.path, flags)
                .map_err(|e| DbError::connection(format!("{}: {e}", settings.path.display())))?;
            conn.busy_timeout(settings.busy_timeout.0)
                .map_err(|e| error(&e))?;
            Ok(Arc::new(Mutex::new(conn)))
        })
        .await
        .map_err(|e| DbError::connection(format!("SQLite task failed: {e}")))?
    }

    async fn fetch(
        &self,
        conn: &mut Shared,
        statement: &Prepared,
        params: &[Param],
        max_rows: usize,
    ) -> Result<Vec<Row>, DbError> {
        let sql = statement.sql.clone();
        let params: Vec<Value> = params.iter().map(value).collect();
        blocking(conn, move |conn| {
            let mut stmt = conn.prepare(&sql).map_err(|e| error(&e))?;
            let names: Vec<String> = stmt
                .column_names()
                .iter()
                .map(|n| (*n).to_owned())
                .collect();
            let mut rows = stmt
                .query(rusqlite::params_from_iter(params))
                .map_err(|e| error(&e))?;
            let mut out = Vec::new();
            while out.len() < max_rows {
                let Some(row) = rows.next().map_err(|e| error(&e))? else {
                    break;
                };
                let mut columns = Vec::with_capacity(names.len());
                for (index, name) in names.iter().enumerate() {
                    let value = row.get_ref(index).map_err(|e| error(&e))?;
                    columns.push((name.clone(), cell(value)));
                }
                out.push(Row { columns });
            }
            Ok(out)
        })
        .await
    }

    async fn write(
        &self,
        conn: &mut Shared,
        statements: &[(&Prepared, Vec<Param>)],
    ) -> Result<u64, DbError> {
        let work: Vec<(String, Vec<Value>)> = statements
            .iter()
            .map(|(statement, params)| (statement.sql.clone(), params.iter().map(value).collect()))
            .collect();
        blocking(conn, move |conn| {
            let tx = conn.transaction().map_err(|e| error(&e))?;
            let mut affected = 0u64;
            for (sql, params) in work {
                let rows = tx
                    .execute(&sql, rusqlite::params_from_iter(params))
                    .map_err(|e| error(&e))?;
                affected += rows as u64;
            }
            tx.commit().map_err(|e| error(&e))?;
            Ok(affected)
        })
        .await
    }
}
