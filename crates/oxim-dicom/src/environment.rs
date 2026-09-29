//! State the DICOM components share: the instance index, the worklist,
//! the inboxes of `dicom-retrieved` sources and pending storage commitment
//! requests.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use oxim_model::MessageId;
use rusqlite::Connection;
use tokio::sync::{mpsc, oneshot};

use crate::assoc::Failure;
use crate::error::DicomError;
use crate::index::IndexStore;
use crate::worklist::WorklistStore;

/// Opens a SQLite database for derived data, or an in-memory one.
pub(crate) fn open_database(path: Option<&Path>, schema: &str) -> Result<Connection, DicomError> {
    let failure = |e: rusqlite::Error| DicomError::Invalid(format!("database: {e}"));
    let connection = match path {
        Some(path) => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    DicomError::Invalid(format!("cannot create {}: {e}", parent.display()))
                })?;
            }
            Connection::open(path).map_err(failure)?
        }
        None => Connection::open_in_memory().map_err(failure)?,
    };
    let _mode: String = connection
        .pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))
        .map_err(failure)?;
    connection
        .pragma_update(None, "synchronous", "NORMAL")
        .map_err(failure)?;
    connection
        .busy_timeout(Duration::from_secs(10))
        .map_err(failure)?;
    connection.execute_batch(schema).map_err(failure)?;
    Ok(connection)
}

/// A store opened on first use; a failed open is retried on the next use.
struct Lazy<T> {
    slot: Mutex<Option<Arc<T>>>,
}

impl<T> Lazy<T> {
    fn new() -> Self {
        Self {
            slot: Mutex::new(None),
        }
    }

    fn get(&self, open: impl FnOnce() -> Result<T, DicomError>) -> Result<Arc<T>, DicomError> {
        let mut slot = self
            .slot
            .lock()
            .map_err(|_| DicomError::invalid("a DICOM store is unavailable"))?;
        if let Some(store) = slot.as_ref() {
            return Ok(store.clone());
        }
        let store = Arc::new(open()?);
        *slot = Some(store.clone());
        Ok(store)
    }
}

/// An instance handed to a `dicom-retrieved` source.
#[derive(Debug)]
pub(crate) struct Retrieved {
    /// The Part 10 object.
    pub(crate) object: Vec<u8>,
    /// Message metadata.
    pub(crate) metadata: BTreeMap<String, String>,
    /// Where it came from.
    pub(crate) peer: Option<String>,
    /// Answered once the object is stored durably.
    pub(crate) stored: oneshot::Sender<Result<MessageId, String>>,
}

/// What a storage commitment report says about one instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitmentReport {
    /// Instances the peer committed to store.
    pub committed: Vec<String>,
    /// Instances it did not commit to, with the failure reason.
    pub failed: Vec<(String, u16)>,
}

struct Inner {
    data_dir: Option<PathBuf>,
    index: Lazy<IndexStore>,
    worklist: Lazy<WorklistStore>,
    inboxes: Mutex<HashMap<String, mpsc::Sender<Retrieved>>>,
    commitments: Mutex<HashMap<String, oneshot::Sender<CommitmentReport>>>,
}

/// The state the DICOM components of one engine share.
///
/// With a data directory, the instance index (`dicom-index.db`) and the
/// worklist with the performed procedure steps (`worklist.db`) are SQLite
/// files there, opened when first used; without one they live in memory.
#[derive(Clone)]
pub struct DicomEnvironment {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for DicomEnvironment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DicomEnvironment")
            .field("data_dir", &self.inner.data_dir)
            .finish_non_exhaustive()
    }
}

impl DicomEnvironment {
    /// Databases in `data_dir`.
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self::build(Some(data_dir.into()))
    }

    /// Databases in memory, for tests and embedded use.
    pub fn in_memory() -> Self {
        Self::build(None)
    }

    fn build(data_dir: Option<PathBuf>) -> Self {
        Self {
            inner: Arc::new(Inner {
                data_dir,
                index: Lazy::new(),
                worklist: Lazy::new(),
                inboxes: Mutex::new(HashMap::new()),
                commitments: Mutex::new(HashMap::new()),
            }),
        }
    }

    fn path(&self, file: &str) -> Option<PathBuf> {
        self.inner.data_dir.as_ref().map(|dir| dir.join(file))
    }

    /// The instance index.
    pub fn index(&self) -> Result<Arc<IndexStore>, DicomError> {
        let path = self.path("dicom-index.db");
        self.inner.index.get(|| IndexStore::open(path.as_deref()))
    }

    /// The worklist and the performed procedure steps.
    pub fn worklist(&self) -> Result<Arc<WorklistStore>, DicomError> {
        let path = self.path("worklist.db");
        self.inner
            .worklist
            .get(|| WorklistStore::open(path.as_deref()))
    }

    /// Opens the inbox `name`; fails when another source holds it.
    pub(crate) fn open_inbox(&self, name: &str, capacity: usize) -> Result<Inbox, DicomError> {
        let mut inboxes = self
            .inner
            .inboxes
            .lock()
            .map_err(|_| DicomError::invalid("the inboxes are unavailable"))?;
        if inboxes.get(name).is_some_and(|sender| !sender.is_closed()) {
            return Err(DicomError::Invalid(format!(
                "the inbox {name:?} is used by another channel"
            )));
        }
        let (sender, receiver) = mpsc::channel(capacity.max(1));
        inboxes.insert(name.to_owned(), sender);
        Ok(Inbox {
            name: name.to_owned(),
            receiver,
            environment: self.clone(),
        })
    }

    /// Hands an object to the source holding the inbox `name` and waits
    /// until it is stored.
    pub(crate) async fn deliver(
        &self,
        name: &str,
        object: Vec<u8>,
        metadata: BTreeMap<String, String>,
        peer: Option<String>,
        timeout: Duration,
    ) -> Result<MessageId, Failure> {
        let sender = self
            .inner
            .inboxes
            .lock()
            .map_err(|_| Failure::Temporary("the inboxes are unavailable".into()))?
            .get(name)
            .filter(|sender| !sender.is_closed())
            .cloned()
            .ok_or_else(|| {
                Failure::Temporary(format!(
                    "no running dicom-retrieved source has the inbox {name:?}"
                ))
            })?;
        let (stored, answer) = oneshot::channel();
        let retrieved = Retrieved {
            object,
            metadata,
            peer,
            stored,
        };
        let work = async {
            sender
                .send(retrieved)
                .await
                .map_err(|_| Failure::Temporary(format!("the inbox {name:?} closed")))?;
            answer
                .await
                .map_err(|_| Failure::Temporary(format!("the inbox {name:?} closed")))?
                .map_err(|e| Failure::Temporary(format!("the object was not stored: {e}")))
        };
        tokio::time::timeout(timeout, work).await.map_err(|_| {
            Failure::Temporary(format!(
                "the inbox {name:?} did not store the object in time"
            ))
        })?
    }

    /// Registers a pending storage commitment request.
    pub(crate) fn await_commitment(
        &self,
        transaction_uid: &str,
    ) -> Result<oneshot::Receiver<CommitmentReport>, DicomError> {
        let (sender, receiver) = oneshot::channel();
        self.inner
            .commitments
            .lock()
            .map_err(|_| DicomError::invalid("the commitment requests are unavailable"))?
            .insert(transaction_uid.to_owned(), sender);
        Ok(receiver)
    }

    /// Forgets a pending storage commitment request.
    pub(crate) fn forget_commitment(&self, transaction_uid: &str) {
        if let Ok(mut pending) = self.inner.commitments.lock() {
            pending.remove(transaction_uid);
        }
    }

    /// Passes a report to the request waiting for it; returns whether one
    /// was waiting.
    pub(crate) fn complete_commitment(
        &self,
        transaction_uid: &str,
        report: CommitmentReport,
    ) -> bool {
        let waiting = self
            .inner
            .commitments
            .lock()
            .ok()
            .and_then(|mut pending| pending.remove(transaction_uid));
        waiting.is_some_and(|sender| sender.send(report).is_ok())
    }
}

/// The receiving end of an inbox; closing it frees the name.
pub(crate) struct Inbox {
    name: String,
    receiver: mpsc::Receiver<Retrieved>,
    environment: DicomEnvironment,
}

impl Inbox {
    pub(crate) async fn recv(&mut self) -> Option<Retrieved> {
        self.receiver.recv().await
    }
}

impl Drop for Inbox {
    fn drop(&mut self) {
        self.receiver.close();
        if let Ok(mut inboxes) = self.environment.inner.inboxes.lock()
            && inboxes.get(&self.name).is_some_and(mpsc::Sender::is_closed)
        {
            inboxes.remove(&self.name);
        }
    }
}
