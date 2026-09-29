//! What the remote file connectors (`sftp`, `ftp`, `s3`) share: a session
//! with directory operations, a polling source and an atomic writer.
//!
//! Polling source settings (in the connector's `settings`):
//!
//! | Setting | Default | Meaning |
//! |---|---|---|
//! | `directory` | the login directory (bucket root for `s3`) | Remote directory to watch |
//! | `pattern` | `*` | File names to pick up; `*` and `?` wildcards, ASCII case-insensitive |
//! | `poll_interval` | `5s` | Time between directory listings |
//! | `sort` | `name` | Processing order: `name` or `modified` |
//! | `after` | `move` | What to do with a stored file: `move` or `delete` |
//! | `processed_directory` | none | Where stored files are moved (required for `move`) |
//! | `error_directory` | none | Where files larger than `max_file_size` are moved |
//! | `max_file_size` | 64 MiB | Largest accepted file |
//!
//! Destination settings:
//!
//! | Setting | Default | Meaning |
//! |---|---|---|
//! | `directory` | the login directory | Remote directory to write to |
//! | `filename` | `{channel}-{message_id}.{extension}` | File name template, as for the local `file` destination |
//! | `overwrite` | `false` | Replace an existing file with the same name |
//! | `create_directory` | `true` | Create missing directories |
//!
//! A file is picked up once its size and modification time stayed the
//! same between two listings, and removed or moved only after it was stored
//! durably (at-least-once: a file stored just before a crash is picked up
//! again). Files whose names start with `.` are ignored. Writers upload to
//! a hidden temporary name (`.<name>.<random>.tmp`) and rename it, so
//! readers never see partial files; object stores write atomically without
//! a temporary name. Writing the same message again succeeds when the
//! existing file has the same content.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use oxim_connectors::file::{AfterRead, SortOrder, matches, render};
use oxim_core::config::DurationText;
use oxim_core::{
    ConnectorError, DestinationConnector, EngineError, SendError, Settings, SourceConnector,
    SourceContext, SubmitInfo, async_trait,
};
use oxim_model::MessageId;
use oxim_store::Delivery;
use serde::Deserialize;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use crate::util::{join, parse, split};

/// A file in a remote directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteEntry {
    /// The name within the directory.
    pub(crate) name: String,
    /// Size in bytes.
    pub(crate) size: u64,
    /// Modification time in seconds since the Unix epoch, when known.
    pub(crate) modified: Option<i64>,
}

/// An open connection to a remote file system.
#[async_trait]
pub(crate) trait Session: Send {
    /// The regular files of `directory` (`""` is the start directory).
    async fn list(&mut self, directory: &str) -> Result<Vec<RemoteEntry>, String>;
    /// The content of a file.
    async fn read(&mut self, path: &str) -> Result<Vec<u8>, String>;
    /// Creates or replaces a file.
    async fn write(&mut self, path: &str, data: &[u8]) -> Result<(), String>;
    /// Deletes a file.
    async fn remove(&mut self, path: &str) -> Result<(), String>;
    /// Renames a file; the target must not exist.
    async fn rename(&mut self, from: &str, to: &str) -> Result<(), String>;
    /// Whether a file exists.
    async fn exists(&mut self, path: &str) -> Result<bool, String>;
    /// Creates a directory and its parents when missing.
    async fn create_dir_all(&mut self, directory: &str) -> Result<(), String>;
    /// Whether `write` replaces files atomically (object stores), so no
    /// temporary name is needed.
    fn atomic_write(&self) -> bool {
        false
    }
    /// Ends the session politely.
    async fn close(&mut self) {}
}

/// Opens sessions to one remote location.
#[async_trait]
pub(crate) trait Connect: Send + Sync + fmt::Debug + 'static {
    /// Opens a session, including authentication.
    async fn connect(&self) -> Result<Box<dyn Session>, String>;
    /// The location for logs and message metadata, such as
    /// `sftp://lab@files.example.org:22`.
    fn location(&self) -> String;
}

fn default_pattern() -> String {
    "*".to_owned()
}

fn default_poll_interval() -> DurationText {
    DurationText(Duration::from_secs(5))
}

fn default_max_file_size() -> u64 {
    64 * 1024 * 1024
}

fn default_filename() -> String {
    "{channel}-{message_id}.{extension}".to_owned()
}

fn yes() -> bool {
    true
}

/// The setting names of [`PollSettings`].
pub(crate) const POLL_KEYS: &[&str] = &[
    "directory",
    "pattern",
    "poll_interval",
    "sort",
    "after",
    "processed_directory",
    "error_directory",
    "max_file_size",
];

/// The setting names of [`WriteSettings`].
pub(crate) const WRITE_KEYS: &[&str] = &["directory", "filename", "overwrite", "create_directory"];

/// How a remote directory is polled.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PollSettings {
    #[serde(default)]
    pub(crate) directory: String,
    #[serde(default = "default_pattern")]
    pub(crate) pattern: String,
    #[serde(default = "default_poll_interval")]
    pub(crate) poll_interval: DurationText,
    #[serde(default)]
    pub(crate) sort: SortOrder,
    #[serde(default)]
    pub(crate) after: AfterRead,
    #[serde(default)]
    pub(crate) processed_directory: Option<String>,
    #[serde(default)]
    pub(crate) error_directory: Option<String>,
    #[serde(default = "default_max_file_size")]
    pub(crate) max_file_size: u64,
}

impl PollSettings {
    fn check(&self, what: &str) -> Result<(), EngineError> {
        if self.after == AfterRead::Move && self.processed_directory.is_none() {
            return Err(EngineError::Config(format!(
                "{what} with `after: move` needs a processed_directory"
            )));
        }
        if self.poll_interval.0.is_zero() {
            return Err(EngineError::Config(format!(
                "{what}: poll_interval must be positive"
            )));
        }
        Ok(())
    }
}

/// How deliveries are written to a remote directory.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WriteSettings {
    #[serde(default)]
    pub(crate) directory: String,
    #[serde(default = "default_filename")]
    pub(crate) filename: String,
    #[serde(default)]
    pub(crate) overwrite: bool,
    #[serde(default = "yes")]
    pub(crate) create_directory: bool,
}

/// Splits source settings into polling settings and connection settings.
pub(crate) fn poll_settings(
    settings: &Settings,
    what: &str,
) -> Result<(PollSettings, Settings), EngineError> {
    let (poll, rest) = split(settings, POLL_KEYS);
    let poll: PollSettings = parse(&poll, what)?;
    poll.check(what)?;
    Ok((poll, rest))
}

/// Splits destination settings into write settings and connection settings.
pub(crate) fn write_settings(
    settings: &Settings,
    what: &str,
) -> Result<(WriteSettings, Settings), EngineError> {
    let (write, rest) = split(settings, WRITE_KEYS);
    let write: WriteSettings = parse(&write, what)?;
    render(
        &write.filename,
        "channel",
        "destination",
        MessageId::from_parts(0, 0),
        None,
    )
    .map_err(|e| EngineError::Config(format!("{what} filename: {e}")))?;
    Ok((write, rest))
}

/// Moves `path` into `directory`, choosing a free name.
async fn move_into(
    session: &mut dyn Session,
    path: &str,
    directory: &str,
    name: &str,
) -> Result<String, String> {
    session.create_dir_all(directory).await?;
    let (stem, extension) = match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => (stem.to_owned(), format!(".{extension}")),
        _ => (name.to_owned(), String::new()),
    };
    let mut target = join(directory, name);
    let mut counter = 1;
    while session.exists(&target).await? {
        target = join(directory, &format!("{stem}-{counter}{extension}"));
        counter += 1;
    }
    session.rename(path, &target).await?;
    Ok(target)
}

/// Picks up files from a remote directory.
#[derive(Debug)]
pub(crate) struct PollingSource<C> {
    connect: Arc<C>,
    settings: PollSettings,
    kind: &'static str,
}

impl<C: Connect> PollingSource<C> {
    pub(crate) fn new(connect: C, settings: PollSettings, kind: &'static str) -> Self {
        Self {
            connect: Arc::new(connect),
            settings,
            kind,
        }
    }

    /// Processes one stable file. A session error is returned so the caller
    /// reconnects; other problems are logged and the file is retried later.
    async fn process(
        &self,
        context: &SourceContext,
        session: &mut dyn Session,
        entry: &RemoteEntry,
        rejected: &mut HashSet<String>,
    ) -> Result<(), String> {
        let settings = &self.settings;
        let path = join(&settings.directory, &entry.name);
        let location = format!("{}/{path}", self.connect.location());
        if entry.size > settings.max_file_size {
            match &settings.error_directory {
                Some(errors) => {
                    let target = move_into(session, &path, errors, &entry.name).await?;
                    warn!(channel = %context.channel(), file = %location, moved_to = %target, "file too large; moved to the error directory");
                }
                None => {
                    if rejected.insert(path.clone()) {
                        warn!(channel = %context.channel(), file = %location, size = entry.size, "file too large; ignoring it");
                    }
                }
            }
            return Ok(());
        }
        let data = session.read(&path).await?;
        let info = SubmitInfo {
            peer: Some(location.clone()),
            metadata: BTreeMap::from([
                ("file.name".to_owned(), entry.name.clone()),
                ("file.size".to_owned(), data.len().to_string()),
                ("remote.path".to_owned(), path.clone()),
            ]),
            ..SubmitInfo::default()
        };
        let id = match context.submit(data, info).await {
            Ok(id) => id,
            Err(e) => {
                warn!(channel = %context.channel(), file = %location, error = %e, "file could not be stored; will retry");
                return Ok(());
            }
        };
        let result = match settings.after {
            AfterRead::Delete => session.remove(&path).await.map(|()| None),
            AfterRead::Move => match &settings.processed_directory {
                Some(processed) => move_into(session, &path, processed, &entry.name)
                    .await
                    .map(Some),
                None => Ok(None),
            },
        };
        match result {
            Ok(target) => {
                debug!(channel = %context.channel(), file = %location, %id, moved_to = ?target, "file stored");
                Ok(())
            }
            Err(e) => {
                warn!(channel = %context.channel(), file = %location, %id, error = %e, "file stored but not removed; it will be picked up again");
                Err(e)
            }
        }
    }

    async fn scan(
        &self,
        context: &SourceContext,
        session: &mut dyn Session,
        seen: &mut HashMap<String, (u64, Option<i64>)>,
        rejected: &mut HashSet<String>,
    ) -> Result<(), String> {
        let settings = &self.settings;
        let entries: Vec<RemoteEntry> = session
            .list(&settings.directory)
            .await?
            .into_iter()
            .filter(|entry| !entry.name.starts_with('.') && matches(&settings.pattern, &entry.name))
            .collect();
        let mut stable: Vec<RemoteEntry> = entries
            .iter()
            .filter(|entry| seen.get(&entry.name) == Some(&(entry.size, entry.modified)))
            .cloned()
            .collect();
        *seen = entries
            .into_iter()
            .map(|entry| (entry.name, (entry.size, entry.modified)))
            .collect();
        match settings.sort {
            SortOrder::Name => stable.sort_by(|a, b| a.name.cmp(&b.name)),
            SortOrder::Modified => {
                stable.sort_by(|a, b| a.modified.cmp(&b.modified).then(a.name.cmp(&b.name)));
            }
        }
        for entry in &stable {
            if context.is_cancelled() {
                return Ok(());
            }
            seen.remove(&entry.name);
            self.process(context, session, entry, rejected).await?;
        }
        rejected.retain(|path| {
            seen.keys()
                .any(|name| join(&settings.directory, name) == *path)
        });
        Ok(())
    }
}

#[async_trait]
impl<C: Connect> SourceConnector for PollingSource<C> {
    async fn run(&self, context: SourceContext) -> Result<(), ConnectorError> {
        let location = self.connect.location();
        info!(channel = %context.channel(), kind = self.kind, %location, directory = %self.settings.directory, "watching remote directory");
        let mut session: Option<Box<dyn Session>> = None;
        let mut seen = HashMap::new();
        let mut rejected = HashSet::new();
        let mut failing = false;
        loop {
            if session.is_none() {
                match self.connect.connect().await {
                    Ok(opened) => {
                        if failing {
                            info!(channel = %context.channel(), %location, "connected again");
                        }
                        failing = false;
                        session = Some(opened);
                    }
                    Err(e) => {
                        if !failing {
                            warn!(channel = %context.channel(), %location, error = %e, "cannot connect; retrying every poll interval");
                        }
                        failing = true;
                    }
                }
            }
            if let Some(open) = session.as_mut() {
                let result = self
                    .scan(&context, open.as_mut(), &mut seen, &mut rejected)
                    .await;
                if let Err(e) = result {
                    warn!(channel = %context.channel(), %location, error = %e, "remote operation failed; reconnecting");
                    if let Some(mut broken) = session.take() {
                        broken.close().await;
                    }
                }
            }
            tokio::select! {
                () = context.cancelled() => break,
                () = tokio::time::sleep(self.settings.poll_interval.0) => {}
            }
        }
        if let Some(mut open) = session.take() {
            open.close().await;
        }
        Ok(())
    }
}

/// What went wrong while writing, and whether the session is still usable.
enum WriteError {
    /// The session failed; reconnect.
    Session(String),
    /// The delivery cannot succeed as it is.
    Permanent(String),
}

/// Writes each delivery to a file in a remote directory.
#[derive(Debug)]
pub(crate) struct RemoteDestination<C> {
    connect: C,
    settings: WriteSettings,
    session: Mutex<Option<Box<dyn Session>>>,
}

impl fmt::Debug for dyn Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Session")
    }
}

impl<C: Connect> RemoteDestination<C> {
    pub(crate) fn new(connect: C, settings: WriteSettings) -> Self {
        Self {
            connect,
            settings,
            session: Mutex::new(None),
        }
    }

    async fn write(
        &self,
        session: &mut dyn Session,
        delivery: &Delivery,
    ) -> Result<(), WriteError> {
        let settings = &self.settings;
        let name = render(
            &settings.filename,
            delivery.channel.as_str(),
            delivery.destination.as_str(),
            delivery.message_id,
            delivery.data_type,
        )
        .map_err(WriteError::Permanent)?;
        let directory = &settings.directory;
        if settings.create_directory && !directory.is_empty() {
            session
                .create_dir_all(directory)
                .await
                .map_err(WriteError::Session)?;
        }
        let target = join(directory, &name);
        let exists = session.exists(&target).await.map_err(WriteError::Session)?;
        if exists && !settings.overwrite {
            let existing = session.read(&target).await.map_err(WriteError::Session)?;
            return if existing == delivery.payload {
                Ok(())
            } else {
                Err(WriteError::Permanent(format!(
                    "{target} already exists with different content"
                )))
            };
        }
        if session.atomic_write() {
            return session
                .write(&target, &delivery.payload)
                .await
                .map_err(WriteError::Session);
        }
        let temporary = join(
            directory,
            &format!(".{name}.{:016x}.tmp", fastrand::u64(..)),
        );
        if let Err(e) = session.write(&temporary, &delivery.payload).await {
            let _ = session.remove(&temporary).await;
            return Err(WriteError::Session(e));
        }
        if exists {
            session.remove(&target).await.map_err(WriteError::Session)?;
        }
        if let Err(e) = session.rename(&temporary, &target).await {
            let _ = session.remove(&temporary).await;
            return Err(WriteError::Session(e));
        }
        Ok(())
    }
}

#[async_trait]
impl<C: Connect> DestinationConnector for RemoteDestination<C> {
    async fn send(&self, delivery: &Delivery) -> Result<Option<Vec<u8>>, SendError> {
        let mut slot = self.session.lock().await;
        // A kept session may have been closed by the server while idle:
        // one fresh connection is tried before the attempt fails.
        let reused = slot.is_some();
        for attempt in 0..2 {
            if slot.is_none() {
                let opened = self.connect.connect().await.map_err(|e| {
                    SendError::temporary(format!("{}: {e}", self.connect.location()))
                })?;
                *slot = Some(opened);
            }
            let Some(session) = slot.as_mut() else {
                return Err(SendError::temporary("no session"));
            };
            match self.write(session.as_mut(), delivery).await {
                Ok(()) => return Ok(None),
                Err(WriteError::Permanent(message)) => return Err(SendError::permanent(message)),
                Err(WriteError::Session(message)) => {
                    if let Some(mut broken) = slot.take() {
                        broken.close().await;
                    }
                    if attempt == 1 || !reused {
                        return Err(SendError::temporary(format!(
                            "{}: {message}",
                            self.connect.location()
                        )));
                    }
                    debug!(location = %self.connect.location(), error = %message, "session lost; reconnecting once");
                }
            }
        }
        Err(SendError::temporary("no session"))
    }
}

/// Test support: an in-memory file system implementing [`Session`].
#[cfg(test)]
pub(crate) mod memory {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    use super::*;

    /// Shared files by path.
    #[derive(Debug, Clone, Default)]
    pub(crate) struct Files(pub(crate) Arc<Mutex<BTreeMap<String, Vec<u8>>>>);

    #[async_trait]
    impl Session for Files {
        async fn list(&mut self, directory: &str) -> Result<Vec<RemoteEntry>, String> {
            let files = self.0.lock().map_err(|e| e.to_string())?;
            let prefix = join(directory, "");
            Ok(files
                .iter()
                .filter_map(|(path, data)| {
                    let name = path.strip_prefix(&prefix)?;
                    (!name.contains('/')).then(|| RemoteEntry {
                        name: name.to_owned(),
                        size: data.len() as u64,
                        modified: Some(0),
                    })
                })
                .collect())
        }
        async fn read(&mut self, path: &str) -> Result<Vec<u8>, String> {
            self.0
                .lock()
                .map_err(|e| e.to_string())?
                .get(path)
                .cloned()
                .ok_or_else(|| format!("{path}: no such file"))
        }
        async fn write(&mut self, path: &str, data: &[u8]) -> Result<(), String> {
            self.0
                .lock()
                .map_err(|e| e.to_string())?
                .insert(path.to_owned(), data.to_vec());
            Ok(())
        }
        async fn remove(&mut self, path: &str) -> Result<(), String> {
            self.0
                .lock()
                .map_err(|e| e.to_string())?
                .remove(path)
                .map(|_| ())
                .ok_or_else(|| format!("{path}: no such file"))
        }
        async fn rename(&mut self, from: &str, to: &str) -> Result<(), String> {
            let mut files = self.0.lock().map_err(|e| e.to_string())?;
            if files.contains_key(to) {
                return Err(format!("{to} exists"));
            }
            let data = files
                .remove(from)
                .ok_or_else(|| format!("{from}: no such file"))?;
            files.insert(to.to_owned(), data);
            Ok(())
        }
        async fn exists(&mut self, path: &str) -> Result<bool, String> {
            Ok(self.0.lock().map_err(|e| e.to_string())?.contains_key(path))
        }
        async fn create_dir_all(&mut self, _directory: &str) -> Result<(), String> {
            Ok(())
        }
    }

    #[derive(Debug, Clone, Default)]
    pub(crate) struct Memory(pub(crate) Files);

    #[async_trait]
    impl Connect for Memory {
        async fn connect(&self) -> Result<Box<dyn Session>, String> {
            Ok(Box::new(self.0.clone()))
        }
        fn location(&self) -> String {
            "memory://".to_owned()
        }
    }
}

#[cfg(test)]
mod tests {
    use oxim_model::{ChannelId, ConnectorId, DataType};

    use super::memory::{Files, Memory};
    use super::*;

    fn delivery(payload: &[u8]) -> Delivery {
        Delivery {
            message_id: MessageId::from_parts(1_790_000_000_000, 7),
            channel: ChannelId::new("lab").unwrap(),
            destination: ConnectorId::new("out").unwrap(),
            attempts: 0,
            payload: payload.to_vec(),
            data_type: Some(DataType::Hl7V2),
        }
    }

    #[tokio::test]
    async fn writes_atomically_and_idempotently() {
        let files = Files::default();
        let destination = RemoteDestination::new(
            Memory(files.clone()),
            WriteSettings {
                directory: "out".into(),
                filename: "{message_id}.{extension}".into(),
                overwrite: false,
                create_directory: true,
            },
        );
        destination.send(&delivery(b"MSH|1")).await.unwrap();
        // A retry of the same delivery succeeds.
        destination.send(&delivery(b"MSH|1")).await.unwrap();
        let written = files.0.lock().unwrap().clone();
        assert_eq!(written.len(), 1);
        let (path, data) = written.iter().next().unwrap();
        assert!(path.starts_with("out/") && path.ends_with(".hl7"), "{path}");
        assert_eq!(data, b"MSH|1");
        // Different content under the same name fails permanently.
        let error = destination.send(&delivery(b"MSH|2")).await.unwrap_err();
        assert!(error.to_string().contains("different content"), "{error}");
    }

    #[test]
    fn checks_settings() {
        let settings: Settings =
            serde_json::from_str(r#"{"host":"h","directory":"in","after":"move"}"#).unwrap();
        assert!(poll_settings(&settings, "test source").is_err());
        let settings: Settings = serde_json::from_str(
            r#"{"host":"h","directory":"in","after":"move","processed_directory":"done"}"#,
        )
        .unwrap();
        let (poll, rest) = poll_settings(&settings, "test source").unwrap();
        assert_eq!(poll.directory, "in");
        assert_eq!(rest.len(), 1);
        let settings: Settings = serde_json::from_str(r#"{"filename":"../x"}"#).unwrap();
        assert!(write_settings(&settings, "test destination").is_err());
    }
}
