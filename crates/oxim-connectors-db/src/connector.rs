//! The polling reader and the writer, shared by every database.

use std::sync::Arc;

use oxim_connectors::values::DeliveryValues;
use oxim_core::{
    ConnectorError, DestinationConnector, SendError, SourceConnector, SourceContext, SubmitInfo,
    async_trait,
};
use oxim_store::Delivery;
use tokio::sync::Mutex;
use tracing::{debug, warn};

use crate::settings::{ParamSource, Reader, Writer};
use crate::sql::{Placeholder, Prepared};
use crate::value::{Param, Row};

/// How a database error affects a delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ErrorKind {
    /// The connection is lost or could not be opened: reconnect and retry.
    Connection,
    /// The database rejected the data (constraint or data error): repeating
    /// the same statement cannot succeed.
    Rejected,
    /// Anything else (for example a deadlock or a missing table): retry.
    Other,
}

/// A database error, already free of connection secrets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DbError {
    pub(crate) kind: ErrorKind,
    pub(crate) message: String,
}

impl DbError {
    pub(crate) fn connection(message: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::Connection,
            message: message.into(),
        }
    }

    pub(crate) fn rejected(message: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::Rejected,
            message: message.into(),
        }
    }

    pub(crate) fn other(message: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::Other,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for DbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// One database: how to connect, query and write.
#[async_trait]
pub(crate) trait Database: std::fmt::Debug + Send + Sync + 'static {
    /// An open connection.
    type Conn: Send + 'static;

    /// The placeholder style of the database.
    fn style(&self) -> Placeholder;

    /// Where the database is, for logs: never contains secrets.
    fn describe(&self) -> String;

    /// Opens a connection.
    async fn connect(&self) -> Result<Self::Conn, DbError>;

    /// Runs a query and returns at most `max_rows` rows.
    async fn fetch(
        &self,
        conn: &mut Self::Conn,
        statement: &Prepared,
        params: &[Param],
        max_rows: usize,
    ) -> Result<Vec<Row>, DbError>;

    /// Runs statements in one transaction; returns the affected rows.
    async fn write(
        &self,
        conn: &mut Self::Conn,
        statements: &[(&Prepared, Vec<Param>)],
    ) -> Result<u64, DbError>;
}

/// Reads new rows at an interval.
#[derive(Debug)]
pub(crate) struct DbSource<D: Database> {
    db: Arc<D>,
    reader: Arc<Reader>,
}

impl<D: Database> DbSource<D> {
    pub(crate) fn new(db: D, reader: Reader) -> Self {
        Self {
            db: Arc::new(db),
            reader: Arc::new(reader),
        }
    }

    /// The payload of a row: its JSON, or one column's content.
    fn payload(&self, row: &Row) -> Result<Vec<u8>, DbError> {
        match &self.reader.column {
            Some(column) => row
                .get(column)
                .ok_or_else(|| DbError::other(format!("the query returns no column {column}")))
                .map(|cell| cell.to_bytes().unwrap_or_default()),
            None => serde_json::to_vec(&row.to_json())
                .map_err(|e| DbError::other(format!("row to JSON: {e}"))),
        }
    }

    /// The parameters of the post query for a row: its columns, then the
    /// constants.
    fn post_params(&self, statement: &Prepared, row: &Row) -> Result<Vec<Param>, DbError> {
        statement
            .names
            .iter()
            .map(|name| match row.get(name) {
                Some(cell) => Ok(Param::text(cell.to_text())),
                None => self
                    .reader
                    .constants
                    .get(name)
                    .map(|value| Param::Text(value.clone()))
                    .ok_or_else(|| {
                        DbError::other(format!(
                            "post_query parameter :{name} is neither a column of the row nor in params"
                        ))
                    }),
            })
            .collect()
    }

    /// One poll: returns how many rows were stored.
    async fn poll(&self, conn: &mut D::Conn, context: &SourceContext) -> Result<usize, DbError> {
        let reader = &self.reader;
        let rows = self
            .db
            .fetch(conn, &reader.query, &reader.query_params, reader.max_rows)
            .await?;
        let mut stored = 0;
        for row in rows {
            if context.is_cancelled() {
                break;
            }
            let payload = self.payload(&row)?;
            let info = SubmitInfo {
                peer: Some(self.db.describe()),
                ..SubmitInfo::default()
            };
            context
                .submit(payload, info)
                .await
                .map_err(|e| DbError::other(format!("the row could not be stored: {e}")))?;
            stored += 1;
            // The row is durable; now mark it. A failure here leaves the row
            // to be read again (at-least-once).
            if let Some(post) = &reader.post_query {
                let params = self.post_params(post, &row)?;
                self.db.write(conn, &[(post, params)]).await?;
            }
        }
        Ok(stored)
    }
}

#[async_trait]
impl<D: Database> SourceConnector for DbSource<D> {
    async fn run(&self, context: SourceContext) -> Result<(), ConnectorError> {
        let mut conn: Option<D::Conn> = None;
        loop {
            if context.is_cancelled() {
                return Ok(());
            }
            let mut again = false;
            if conn.is_none() {
                match self.db.connect().await {
                    Ok(opened) => {
                        debug!(channel = %context.channel(), database = %self.db.describe(), "connected");
                        conn = Some(opened);
                    }
                    Err(error) => {
                        warn!(channel = %context.channel(), database = %self.db.describe(), %error, "cannot connect");
                    }
                }
            }
            if let Some(open) = conn.as_mut() {
                match self.poll(open, &context).await {
                    Ok(stored) => {
                        if stored > 0 {
                            debug!(channel = %context.channel(), rows = stored, "stored rows");
                        }
                        // A full batch suggests more rows are waiting.
                        again = stored >= self.reader.max_rows;
                    }
                    Err(error) => {
                        warn!(channel = %context.channel(), database = %self.db.describe(), %error, "poll failed");
                        if error.kind == ErrorKind::Connection {
                            conn = None;
                        }
                    }
                }
            }
            if !again {
                tokio::select! {
                    () = context.cancelled() => return Ok(()),
                    () = tokio::time::sleep(self.reader.interval) => {}
                }
            }
        }
    }
}

/// Runs statements for each delivery.
pub(crate) struct DbDestination<D: Database> {
    db: D,
    writer: Writer,
    conn: Mutex<Option<D::Conn>>,
}

impl<D: Database> std::fmt::Debug for DbDestination<D> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DbDestination")
            .field("db", &self.db)
            .field("writer", &self.writer)
            .finish_non_exhaustive()
    }
}

impl<D: Database> DbDestination<D> {
    pub(crate) fn new(db: D, writer: Writer) -> Self {
        Self {
            db,
            writer,
            conn: Mutex::new(None),
        }
    }

    /// The parameters of every statement for a delivery.
    fn bind(&self, delivery: &Delivery) -> Result<Vec<Vec<Param>>, String> {
        let mut values = DeliveryValues::new(delivery);
        let mut resolved: std::collections::BTreeMap<&str, Param> =
            std::collections::BTreeMap::new();
        for (name, source) in &self.writer.params {
            let param = match source {
                ParamSource::PayloadBytes => Param::Bytes(delivery.payload.clone()),
                ParamSource::Value(source) => Param::text(values.get(source)?),
            };
            resolved.insert(name, param);
        }
        Ok(self
            .writer
            .statements
            .iter()
            .map(|statement| {
                statement
                    .names
                    .iter()
                    .map(|name| resolved.get(name.as_str()).cloned().unwrap_or(Param::Null))
                    .collect()
            })
            .collect())
    }
}

#[async_trait]
impl<D: Database> DestinationConnector for DbDestination<D> {
    async fn send(&self, delivery: &Delivery) -> Result<Option<Vec<u8>>, SendError> {
        let params = self.bind(delivery).map_err(|e| {
            SendError::permanent(format!("cannot bind the statement parameters: {e}"))
        })?;
        let statements: Vec<(&Prepared, Vec<Param>)> =
            self.writer.statements.iter().zip(params).collect();
        let mut slot = self.conn.lock().await;
        if slot.is_none() {
            let opened = self.db.connect().await.map_err(|e| {
                SendError::temporary(format!("cannot connect to {}: {e}", self.db.describe()))
            })?;
            *slot = Some(opened);
        }
        let Some(conn) = slot.as_mut() else {
            return Err(SendError::temporary("no connection"));
        };
        match self.db.write(conn, &statements).await {
            Ok(rows) => Ok(Some(format!("{rows} rows affected").into_bytes())),
            Err(error) => {
                if error.kind == ErrorKind::Connection {
                    *slot = None;
                }
                Err(match error.kind {
                    ErrorKind::Rejected => SendError::permanent(error.message),
                    ErrorKind::Connection | ErrorKind::Other => SendError::temporary(error.message),
                })
            }
        }
    }
}
