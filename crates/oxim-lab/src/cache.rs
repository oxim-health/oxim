//! The order cache: tests requested per specimen, kept so analyzers can be
//! answered when they query by barcode, even while the LIS is unreachable.

use std::fmt;
use std::path::Path;
use std::str::FromStr;
use std::sync::Mutex;

use oxim_model::{CodeableConcept, Order, OrderControl, OrderGroup, Patient, Specimen, Timestamp};
use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;

/// Errors of the order cache.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CacheError {
    /// The database reported an error.
    #[error("order cache database: {0}")]
    Database(#[from] rusqlite::Error),
    /// A stored value could not be decoded.
    #[error("corrupt order cache entry: {0}")]
    Corrupt(String),
    /// An order carries no specimen identifier to file it under.
    #[error("the order has no specimen identifier")]
    MissingSpecimen,
    /// The cache lock was poisoned by a panic in another thread.
    #[error("the order cache is unavailable")]
    Unavailable,
}

/// Result alias for cache operations.
pub type CacheResult<T> = Result<T, CacheError>;

/// Where a requested test stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TestStatus {
    /// Requested, not yet given to a device.
    Pending,
    /// Given to a device (worklist or query answer).
    Sent,
    /// A result was received.
    Resulted,
    /// Cancelled by the requester.
    Cancelled,
}

impl TestStatus {
    /// The stored name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Sent => "sent",
            Self::Resulted => "resulted",
            Self::Cancelled => "cancelled",
        }
    }

    /// Whether a device should still perform the test.
    pub fn is_open(self) -> bool {
        matches!(self, Self::Pending | Self::Sent)
    }
}

impl FromStr for TestStatus {
    type Err = CacheError;

    fn from_str(s: &str) -> CacheResult<Self> {
        Ok(match s {
            "pending" => Self::Pending,
            "sent" => Self::Sent,
            "resulted" => Self::Resulted,
            "cancelled" => Self::Cancelled,
            other => return Err(CacheError::Corrupt(format!("test status {other:?}"))),
        })
    }
}

impl fmt::Display for TestStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One requested test of a cached order.
#[derive(Debug, Clone, PartialEq)]
pub struct CachedTest {
    /// The primary code, the key within the order.
    pub code: String,
    /// The test as requested.
    pub test: CodeableConcept,
    /// Where the test stands.
    pub status: TestStatus,
    /// The device the test was last given to.
    pub device: Option<String>,
    /// When the status last changed.
    pub updated_at: Timestamp,
}

/// Everything cached for one specimen.
#[derive(Debug, Clone, PartialEq)]
pub struct CachedOrder {
    /// The specimen identifier (usually the tube barcode).
    pub specimen_id: String,
    /// The patient, if the order carried one.
    pub patient: Option<Patient>,
    /// The specimen details.
    pub specimen: Option<Specimen>,
    /// Order details other than the tests (placer and filler numbers,
    /// priority, request time).
    pub order: Order,
    /// The requested tests.
    pub tests: Vec<CachedTest>,
    /// When the order was first cached.
    pub received_at: Timestamp,
    /// When the order last changed.
    pub updated_at: Timestamp,
}

impl CachedOrder {
    /// The order as a normalized group with the tests `keep` accepts.
    pub fn to_group(&self, keep: impl Fn(&CachedTest) -> bool) -> OrderGroup {
        let mut order = self.order.clone();
        order.tests = self
            .tests
            .iter()
            .filter(|test| keep(test))
            .map(|test| test.test.clone())
            .collect();
        if !order.specimen_ids.contains(&self.specimen_id) {
            order.specimen_ids.insert(0, self.specimen_id.clone());
        }
        OrderGroup {
            patient: self.patient.clone(),
            specimen: self.specimen.clone(),
            order,
        }
    }
}

/// What [`OrderCache::apply`] changed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApplyOutcome {
    /// The specimen the order was filed under.
    pub specimen_id: String,
    /// Tests added.
    pub added: usize,
    /// Tests cancelled.
    pub cancelled: usize,
}

/// The primary code of a test, used as its key.
pub fn test_code(test: &CodeableConcept) -> Option<&str> {
    test.primary_code().or(test.text.as_deref())
}

/// The specimen identifier of an order group: the specimen's first
/// identifier, else the order's first specimen reference.
pub fn specimen_id(group: &OrderGroup) -> Option<&str> {
    group
        .specimen
        .as_ref()
        .and_then(|specimen| specimen.identifiers.first())
        .map(|identifier| identifier.value.as_str())
        .or_else(|| group.order.specimen_ids.first().map(String::as_str))
        .filter(|id| !id.trim().is_empty())
}

/// A durable cache of orders by specimen identifier.
///
/// The cache is derived data: it can always be rebuilt by reprocessing the
/// stored order messages, so it favors speed (`synchronous=NORMAL`).
#[derive(Debug)]
pub struct OrderCache {
    conn: Mutex<Connection>,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS orders (
    specimen_id TEXT PRIMARY KEY NOT NULL,
    patient TEXT,
    specimen TEXT,
    details TEXT NOT NULL,
    received_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS orders_by_update ON orders (updated_at);
CREATE TABLE IF NOT EXISTS order_tests (
    specimen_id TEXT NOT NULL REFERENCES orders (specimen_id) ON DELETE CASCADE,
    code TEXT NOT NULL,
    test TEXT NOT NULL,
    status TEXT NOT NULL,
    device TEXT,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (specimen_id, code)
);
";

fn json<T: serde::Serialize>(value: &T) -> CacheResult<String> {
    serde_json::to_string(value).map_err(|e| CacheError::Corrupt(e.to_string()))
}

fn from_json<T: serde::de::DeserializeOwned>(text: &str) -> CacheResult<T> {
    serde_json::from_str(text).map_err(|e| CacheError::Corrupt(e.to_string()))
}

impl OrderCache {
    /// Opens or creates the cache at `path`.
    pub fn open(path: impl AsRef<Path>) -> CacheResult<Self> {
        Self::prepare(Connection::open(path)?)
    }

    /// A private in-memory cache, for tests.
    pub fn open_in_memory() -> CacheResult<Self> {
        Self::prepare(Connection::open_in_memory()?)
    }

    fn prepare(conn: Connection) -> CacheResult<Self> {
        let _mode: String =
            conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn with<T>(&self, work: impl FnOnce(&mut Connection) -> CacheResult<T>) -> CacheResult<T> {
        let mut conn = self.conn.lock().map_err(|_| CacheError::Unavailable)?;
        work(&mut conn)
    }

    /// Files an order group. `New` adds tests that are not cached yet
    /// (known tests keep their status, so a retransmitted order changes
    /// nothing), `Add` does the same and also requests resulted tests again
    /// (reruns and reflex tests), `Replace` makes the open tests exactly
    /// the given ones, and `Cancel` cancels the given tests or, when none
    /// are given, every open test. A cancelled test that is ordered again
    /// is pending again.
    pub fn apply(&self, group: &OrderGroup, now: Timestamp) -> CacheResult<ApplyOutcome> {
        let specimen_id = specimen_id(group)
            .ok_or(CacheError::MissingSpecimen)?
            .to_owned();
        let control = group.order.control.unwrap_or(OrderControl::New);
        let mut details = group.order.clone();
        details.tests.clear();
        let patient = group.patient.as_ref().map(json).transpose()?;
        let specimen = group.specimen.as_ref().map(json).transpose()?;
        let details = json(&details)?;
        let at = now.unix_nanos();
        self.with(|conn| {
            let tx = conn.transaction()?;
            let mut outcome = ApplyOutcome {
                specimen_id: specimen_id.clone(),
                ..ApplyOutcome::default()
            };
            if control != OrderControl::Cancel {
                tx.execute(
                    "INSERT INTO orders (specimen_id, patient, specimen, details, received_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?5)
                     ON CONFLICT (specimen_id) DO UPDATE SET
                        patient = COALESCE(excluded.patient, orders.patient),
                        specimen = COALESCE(excluded.specimen, orders.specimen),
                        details = excluded.details,
                        updated_at = excluded.updated_at",
                    params![specimen_id, patient, specimen, details, at],
                )?;
            }
            let codes: Vec<(String, String)> = group
                .order
                .tests
                .iter()
                .filter_map(|test| Some((test_code(test)?.to_owned(), json(test).ok()?)))
                .collect();
            match control {
                OrderControl::New | OrderControl::Add | OrderControl::Replace => {
                    if control == OrderControl::Replace {
                        let keep: Vec<&str> = codes.iter().map(|(code, _)| code.as_str()).collect();
                        let mut open = tx.prepare(
                            "SELECT code FROM order_tests
                             WHERE specimen_id = ?1 AND status IN ('pending', 'sent')",
                        )?;
                        let existing = open
                            .query_map([&specimen_id], |row| row.get::<_, String>(0))?
                            .collect::<Result<Vec<_>, _>>()?;
                        drop(open);
                        for code in existing.iter().filter(|code| !keep.contains(&code.as_str())) {
                            outcome.cancelled += tx.execute(
                                "UPDATE order_tests SET status = 'cancelled', updated_at = ?3
                                 WHERE specimen_id = ?1 AND code = ?2",
                                params![specimen_id, code, at],
                            )?;
                        }
                    }
                    // Reruns: `Add` requests resulted tests again.
                    let reopen = if control == OrderControl::Add {
                        "order_tests.status IN ('cancelled', 'resulted')"
                    } else {
                        "order_tests.status = 'cancelled'"
                    };
                    let upsert = format!(
                        "INSERT INTO order_tests (specimen_id, code, test, status, device, updated_at)
                         VALUES (?1, ?2, ?3, 'pending', NULL, ?4)
                         ON CONFLICT (specimen_id, code) DO UPDATE SET
                            status = 'pending',
                            device = NULL,
                            test = excluded.test,
                            updated_at = excluded.updated_at
                         WHERE {reopen}"
                    );
                    for (code, test) in &codes {
                        outcome.added += tx.execute(&upsert, params![specimen_id, code, test, at])?;
                    }
                }
                OrderControl::Cancel => {
                    if codes.is_empty() {
                        outcome.cancelled += tx.execute(
                            "UPDATE order_tests SET status = 'cancelled', updated_at = ?2
                             WHERE specimen_id = ?1 AND status IN ('pending', 'sent')",
                            params![specimen_id, at],
                        )?;
                    } else {
                        for (code, _) in &codes {
                            outcome.cancelled += tx.execute(
                                "UPDATE order_tests SET status = 'cancelled', updated_at = ?3
                                 WHERE specimen_id = ?1 AND code = ?2 AND status IN ('pending', 'sent')",
                                params![specimen_id, code, at],
                            )?;
                        }
                    }
                    tx.execute(
                        "UPDATE orders SET updated_at = ?2 WHERE specimen_id = ?1",
                        params![specimen_id, at],
                    )?;
                }
            }
            tx.commit()?;
            Ok(outcome)
        })
    }

    /// The cached order of a specimen.
    pub fn get(&self, specimen_id: &str) -> CacheResult<Option<CachedOrder>> {
        self.with(|conn| {
            let row = conn
                .query_row(
                    "SELECT patient, specimen, details, received_at, updated_at
                     FROM orders WHERE specimen_id = ?1",
                    [specimen_id],
                    |row| {
                        Ok((
                            row.get::<_, Option<String>>(0)?,
                            row.get::<_, Option<String>>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, i64>(3)?,
                            row.get::<_, i64>(4)?,
                        ))
                    },
                )
                .optional()?;
            let Some((patient, specimen, details, received_at, updated_at)) = row else {
                return Ok(None);
            };
            let mut statement = conn.prepare_cached(
                "SELECT code, test, status, device, updated_at FROM order_tests
                 WHERE specimen_id = ?1 ORDER BY rowid",
            )?;
            let rows = statement
                .query_map([specimen_id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            drop(statement);
            let tests = rows
                .into_iter()
                .map(|(code, test, status, device, updated)| {
                    Ok(CachedTest {
                        code,
                        test: from_json(&test)?,
                        status: status.parse()?,
                        device,
                        updated_at: Timestamp::from_unix_nanos(updated),
                    })
                })
                .collect::<CacheResult<Vec<_>>>()?;
            Ok(Some(CachedOrder {
                specimen_id: specimen_id.to_owned(),
                patient: patient.as_deref().map(from_json).transpose()?,
                specimen: specimen.as_deref().map(from_json).transpose()?,
                order: from_json(&details)?,
                tests,
                received_at: Timestamp::from_unix_nanos(received_at),
                updated_at: Timestamp::from_unix_nanos(updated_at),
            }))
        })
    }

    /// Assigns an open test that has no device yet to `device` (load
    /// balancing). Returns whether the test was assigned.
    pub fn assign(
        &self,
        specimen_id: &str,
        code: &str,
        device: &str,
        now: Timestamp,
    ) -> CacheResult<bool> {
        self.with(|conn| {
            Ok(conn.execute(
                "UPDATE order_tests SET device = ?3, updated_at = ?4
                 WHERE specimen_id = ?1 AND code = ?2 AND device IS NULL
                   AND status IN ('pending', 'sent')",
                params![specimen_id, code, device, now.unix_nanos()],
            )? > 0)
        })
    }

    /// The number of open tests (pending or sent) assigned to each device.
    pub fn open_tests_by_device(&self) -> CacheResult<std::collections::HashMap<String, u64>> {
        self.with(|conn| {
            let mut statement = conn.prepare_cached(
                "SELECT device, COUNT(*) FROM order_tests
                 WHERE device IS NOT NULL AND status IN ('pending', 'sent')
                 GROUP BY device",
            )?;
            let counts = statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })?
                .map(|row| row.map(|(device, count)| (device, u64::try_from(count).unwrap_or(0))))
                .collect::<Result<_, _>>()?;
            Ok(counts)
        })
    }

    /// Sets the status of tests of a specimen, recording the device.
    /// Returns how many tests changed.
    pub fn set_status(
        &self,
        specimen_id: &str,
        codes: &[&str],
        status: TestStatus,
        device: Option<&str>,
        now: Timestamp,
    ) -> CacheResult<usize> {
        self.with(|conn| {
            let tx = conn.transaction()?;
            let mut changed = 0;
            for code in codes {
                changed += tx.execute(
                    "UPDATE order_tests SET status = ?3, device = COALESCE(?4, device), updated_at = ?5
                     WHERE specimen_id = ?1 AND code = ?2 AND status <> 'cancelled'",
                    params![specimen_id, code, status.as_str(), device, now.unix_nanos()],
                )?;
            }
            tx.commit()?;
            Ok(changed)
        })
    }

    /// Deletes orders not changed since `before`. Returns how many.
    pub fn prune(&self, before: Timestamp) -> CacheResult<u64> {
        self.with(|conn| {
            Ok(conn.execute(
                "DELETE FROM orders WHERE updated_at < ?1",
                [before.unix_nanos()],
            )? as u64)
        })
    }
}

#[cfg(test)]
mod tests {
    use oxim_model::{Coding, Identifier};

    use super::*;

    fn at(seconds: i64) -> Timestamp {
        Timestamp::from_unix_nanos(seconds * 1_000_000_000)
    }

    fn group(specimen: &str, tests: &[&str], control: OrderControl) -> OrderGroup {
        OrderGroup {
            patient: Some(Patient {
                identifiers: vec![Identifier::new("P1")],
                ..Patient::default()
            }),
            specimen: Some(Specimen {
                identifiers: vec![Identifier::new(specimen)],
                ..Specimen::default()
            }),
            order: Order {
                placer_id: Some("ORD1".into()),
                tests: tests
                    .iter()
                    .map(|code| CodeableConcept::from_coding(Coding::new(*code)))
                    .collect(),
                control: Some(control),
                ..Order::default()
            },
        }
    }

    fn statuses(cache: &OrderCache, specimen: &str) -> Vec<(String, TestStatus)> {
        let mut tests: Vec<_> = cache
            .get(specimen)
            .unwrap()
            .unwrap()
            .tests
            .into_iter()
            .map(|test| (test.code, test.status))
            .collect();
        tests.sort_by(|a, b| a.0.cmp(&b.0));
        tests
    }

    #[test]
    fn files_adds_replaces_and_cancels_tests() {
        let cache = OrderCache::open_in_memory().unwrap();
        let outcome = cache
            .apply(&group("S1", &["GLU", "CREA"], OrderControl::New), at(1))
            .unwrap();
        assert_eq!((outcome.specimen_id.as_str(), outcome.added), ("S1", 2));
        cache
            .set_status("S1", &["GLU"], TestStatus::Sent, Some("chem-1"), at(2))
            .unwrap();
        // Adding again keeps the status of known tests.
        cache
            .apply(&group("S1", &["GLU", "K"], OrderControl::Add), at(3))
            .unwrap();
        assert_eq!(
            statuses(&cache, "S1"),
            [
                ("CREA".into(), TestStatus::Pending),
                ("GLU".into(), TestStatus::Sent),
                ("K".into(), TestStatus::Pending)
            ]
        );
        cache
            .apply(&group("S1", &["GLU", "K"], OrderControl::Replace), at(4))
            .unwrap();
        assert_eq!(
            statuses(&cache, "S1")[0],
            ("CREA".into(), TestStatus::Cancelled)
        );
        cache
            .apply(&group("S1", &["K"], OrderControl::Cancel), at(5))
            .unwrap();
        assert_eq!(
            statuses(&cache, "S1")[2],
            ("K".into(), TestStatus::Cancelled)
        );
        cache
            .apply(&group("S1", &[], OrderControl::Cancel), at(6))
            .unwrap();
        assert!(
            statuses(&cache, "S1")
                .iter()
                .all(|(_, s)| *s == TestStatus::Cancelled)
        );
        // A cancelled test can be ordered again.
        cache
            .apply(&group("S1", &["K"], OrderControl::New), at(7))
            .unwrap();
        assert_eq!(statuses(&cache, "S1")[2], ("K".into(), TestStatus::Pending));

        let order = cache.get("S1").unwrap().unwrap();
        assert_eq!(order.order.placer_id.as_deref(), Some("ORD1"));
        assert_eq!(order.patient.unwrap().identifiers[0].value, "P1");
        let open = cache
            .get("S1")
            .unwrap()
            .unwrap()
            .to_group(|t| t.status.is_open());
        assert_eq!(open.order.tests.len(), 1);
        assert_eq!(open.order.specimen_ids, ["S1"]);

        // A retransmitted order leaves a resulted test alone; `Add` reruns it.
        cache
            .set_status("S1", &["K"], TestStatus::Resulted, None, at(8))
            .unwrap();
        cache
            .apply(&group("S1", &["K"], OrderControl::New), at(9))
            .unwrap();
        assert_eq!(
            statuses(&cache, "S1")[2],
            ("K".into(), TestStatus::Resulted)
        );
        cache
            .apply(&group("S1", &["K"], OrderControl::Add), at(10))
            .unwrap();
        assert_eq!(statuses(&cache, "S1")[2], ("K".into(), TestStatus::Pending));
    }

    #[test]
    fn assigns_devices_and_counts_their_work() {
        let cache = OrderCache::open_in_memory().unwrap();
        cache
            .apply(&group("S1", &["GLU", "CREA"], OrderControl::New), at(1))
            .unwrap();
        cache
            .apply(&group("S2", &["GLU"], OrderControl::New), at(1))
            .unwrap();
        assert!(cache.assign("S1", "GLU", "chem-1", at(2)).unwrap());
        assert!(cache.assign("S1", "CREA", "chem-1", at(2)).unwrap());
        assert!(cache.assign("S2", "GLU", "chem-2", at(2)).unwrap());
        // An assigned test keeps its device.
        assert!(!cache.assign("S2", "GLU", "chem-1", at(3)).unwrap());
        let counts = cache.open_tests_by_device().unwrap();
        assert_eq!(counts.get("chem-1"), Some(&2));
        assert_eq!(counts.get("chem-2"), Some(&1));
        cache
            .set_status("S1", &["GLU"], TestStatus::Resulted, None, at(4))
            .unwrap();
        assert_eq!(
            cache.open_tests_by_device().unwrap().get("chem-1"),
            Some(&1)
        );
    }

    #[test]
    fn requires_a_specimen_and_prunes_old_orders() {
        let cache = OrderCache::open_in_memory().unwrap();
        let mut orphan = group("x", &["GLU"], OrderControl::New);
        orphan.specimen = None;
        assert!(matches!(
            cache.apply(&orphan, at(1)),
            Err(CacheError::MissingSpecimen)
        ));
        cache
            .apply(&group("OLD", &["GLU"], OrderControl::New), at(1))
            .unwrap();
        cache
            .apply(&group("NEW", &["GLU"], OrderControl::New), at(100))
            .unwrap();
        assert_eq!(cache.prune(at(50)).unwrap(), 1);
        assert!(cache.get("OLD").unwrap().is_none());
        assert!(cache.get("NEW").unwrap().is_some());
        assert!(cache.get("missing").unwrap().is_none());
    }

    #[test]
    fn survives_reopening() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("orders.db");
        OrderCache::open(&path)
            .unwrap()
            .apply(&group("S9", &["HGB"], OrderControl::New), at(1))
            .unwrap();
        let cache = OrderCache::open(&path).unwrap();
        assert_eq!(cache.get("S9").unwrap().unwrap().tests.len(), 1);
    }
}
