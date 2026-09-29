//! Files in a directory: a poller that picks up files dropped by devices
//! and systems, and a writer that stores each delivery as a file.
//!
//! Source type `file`:
//!
//! | Setting | Default | Meaning |
//! |---|---|---|
//! | `directory` | required | Directory to watch |
//! | `pattern` | `*` | File names to pick up; `*` and `?` wildcards, ASCII case-insensitive |
//! | `poll_interval` | `5s` | Time between directory scans |
//! | `sort` | `name` | Processing order: `name` or `modified` |
//! | `after` | `move` | What to do with a stored file: `move` or `delete` |
//! | `processed_directory` | none | Where stored files are moved (required for `move`) |
//! | `error_directory` | none | Where files that can never be accepted (too large) are moved |
//! | `max_file_size` | 64 MiB | Largest accepted file |
//!
//! A file is picked up only after its size and modification time stayed
//! the same between two scans, so files still being written are left
//! alone. Hidden files (names starting with `.`) are ignored. A file is
//! moved or deleted only after it was stored durably; if OXIM stops between
//! storing a file and moving it, the file is picked up again on restart, so
//! delivery is at-least-once.
//!
//! Destination type `file`:
//!
//! | Setting | Default | Meaning |
//! |---|---|---|
//! | `directory` | required | Directory to write to |
//! | `filename` | `{channel}-{message_id}.{extension}` | File name template |
//! | `overwrite` | `false` | Replace an existing file with the same name |
//! | `create_directory` | `true` | Create the directory when it is missing |
//!
//! Templates may use `{channel}`, `{message_id}`, `{destination}`,
//! `{timestamp}` (the UTC receive time of the message, `20260929T120000Z`)
//! and `{extension}` (derived from the data type, for example `hl7`).
//! Files are written to a hidden temporary file, flushed to disk and then
//! renamed, so readers never see partial files. Writing the same message
//! again (after a retry) succeeds when the existing file has the same
//! content; a different existing file fails the delivery unless
//! `overwrite` is set.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use oxim_core::config::DurationText;
use oxim_core::{
    ConnectorError, DestinationConfig, DestinationConnector, EngineError, Registry, SendError,
    SourceConfig, SourceConnector, SourceContext, SubmitInfo, async_trait,
};
use oxim_model::{ClinicalDateTime, DataType, MessageId, Timestamp};
use oxim_store::Delivery;
use serde::Deserialize;
use tokio::io::AsyncWriteExt;
use tracing::{debug, info, warn};

use crate::net::settings;

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

/// Order in which stable files are processed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortOrder {
    /// By file name.
    #[default]
    Name,
    /// By modification time, oldest first.
    Modified,
}

/// What happens to a file once it is stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AfterRead {
    /// Move it to the processed directory.
    #[default]
    Move,
    /// Delete it.
    Delete,
}

/// Settings of the `file` source.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileSourceSettings {
    /// Directory to watch.
    pub directory: PathBuf,
    /// File name pattern.
    #[serde(default = "default_pattern")]
    pub pattern: String,
    /// Time between scans.
    #[serde(default = "default_poll_interval")]
    pub poll_interval: DurationText,
    /// Processing order.
    #[serde(default)]
    pub sort: SortOrder,
    /// What to do with stored files.
    #[serde(default)]
    pub after: AfterRead,
    /// Where stored files are moved.
    #[serde(default)]
    pub processed_directory: Option<PathBuf>,
    /// Where rejected files are moved.
    #[serde(default)]
    pub error_directory: Option<PathBuf>,
    /// Largest accepted file.
    #[serde(default = "default_max_file_size")]
    pub max_file_size: u64,
}

/// Picks up files from a directory.
#[derive(Debug, Clone)]
pub struct FileSource {
    settings: FileSourceSettings,
}

impl FileSource {
    /// Validates the settings and creates the source.
    pub fn new(settings: FileSourceSettings) -> Result<Self, EngineError> {
        if settings.after == AfterRead::Move && settings.processed_directory.is_none() {
            return Err(EngineError::Config(
                "file source with `after: move` needs a processed_directory".into(),
            ));
        }
        if settings.poll_interval.0.is_zero() {
            return Err(EngineError::Config("poll_interval must be positive".into()));
        }
        Ok(Self { settings })
    }
}

/// A file seen during a scan.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    path: PathBuf,
    name: String,
    size: u64,
    modified: Option<SystemTime>,
}

/// Whether `name` matches `pattern` with `*` and `?` wildcards, ignoring
/// ASCII case.
pub(crate) fn matches(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().map(|c| c.to_ascii_lowercase()).collect();
    let name: Vec<char> = name.chars().map(|c| c.to_ascii_lowercase()).collect();
    let (mut p, mut n) = (0, 0);
    let mut backtrack: Option<(usize, usize)> = None;
    while n < name.len() {
        match pattern.get(p) {
            Some('*') => {
                backtrack = Some((p, n));
                p += 1;
            }
            Some(&c) if c == '?' || c == name[n] => {
                p += 1;
                n += 1;
            }
            _ => match backtrack {
                Some((star, matched)) => {
                    p = star + 1;
                    n = matched + 1;
                    backtrack = Some((star, matched + 1));
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|&c| c == '*')
}

async fn scan(directory: &Path, pattern: &str) -> std::io::Result<Vec<Entry>> {
    let mut entries = Vec::new();
    let mut listing = tokio::fs::read_dir(directory).await?;
    while let Some(item) = listing.next_entry().await? {
        let name = item.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || !matches(pattern, &name) {
            continue;
        }
        let Ok(metadata) = item.metadata().await else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        entries.push(Entry {
            path: item.path(),
            name,
            size: metadata.len(),
            modified: metadata.modified().ok(),
        });
    }
    Ok(entries)
}

/// Moves `path` into `directory`, choosing a free name.
async fn move_into(path: &Path, directory: &Path, name: &str) -> std::io::Result<PathBuf> {
    tokio::fs::create_dir_all(directory).await?;
    let (stem, extension) = match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => (stem.to_owned(), format!(".{extension}")),
        _ => (name.to_owned(), String::new()),
    };
    let mut target = directory.join(name);
    let mut counter = 1;
    while tokio::fs::try_exists(&target).await.unwrap_or(false) {
        target = directory.join(format!("{stem}-{counter}{extension}"));
        counter += 1;
    }
    if tokio::fs::rename(path, &target).await.is_err() {
        // Different volume: copy, then remove the original.
        tokio::fs::copy(path, &target).await?;
        tokio::fs::remove_file(path).await?;
    }
    Ok(target)
}

impl FileSource {
    async fn process(
        &self,
        context: &SourceContext,
        entry: &Entry,
        rejected: &mut HashSet<PathBuf>,
    ) {
        let settings = &self.settings;
        if entry.size > settings.max_file_size {
            match &settings.error_directory {
                Some(errors) => match move_into(&entry.path, errors, &entry.name).await {
                    Ok(target) => {
                        warn!(channel = %context.channel(), file = %entry.path.display(), moved_to = %target.display(), "file too large; moved to the error directory")
                    }
                    Err(e) => {
                        warn!(channel = %context.channel(), file = %entry.path.display(), error = %e, "file too large and cannot be moved")
                    }
                },
                None => {
                    if rejected.insert(entry.path.clone()) {
                        warn!(channel = %context.channel(), file = %entry.path.display(), size = entry.size, "file too large; ignoring it");
                    }
                }
            }
            return;
        }
        let data = match tokio::fs::read(&entry.path).await {
            Ok(data) => data,
            Err(e) => {
                debug!(channel = %context.channel(), file = %entry.path.display(), error = %e, "cannot read file yet");
                return;
            }
        };
        let info = SubmitInfo {
            peer: Some(entry.path.display().to_string()),
            metadata: BTreeMap::from([
                ("file.name".to_owned(), entry.name.clone()),
                ("file.size".to_owned(), data.len().to_string()),
            ]),
            ..SubmitInfo::default()
        };
        let id = match context.submit(data, info).await {
            Ok(id) => id,
            Err(e) => {
                warn!(channel = %context.channel(), file = %entry.path.display(), error = %e, "file could not be stored; will retry");
                return;
            }
        };
        let result = match settings.after {
            AfterRead::Delete => tokio::fs::remove_file(&entry.path).await.map(|()| None),
            AfterRead::Move => match &settings.processed_directory {
                Some(processed) => move_into(&entry.path, processed, &entry.name)
                    .await
                    .map(Some),
                None => Ok(None),
            },
        };
        match result {
            Ok(target) => {
                debug!(channel = %context.channel(), file = %entry.path.display(), %id, moved_to = ?target, "file stored")
            }
            Err(e) => {
                warn!(channel = %context.channel(), file = %entry.path.display(), %id, error = %e, "file stored but not removed; it will be picked up again")
            }
        }
    }
}

#[async_trait]
impl SourceConnector for FileSource {
    async fn run(&self, context: SourceContext) -> Result<(), ConnectorError> {
        let settings = &self.settings;
        for directory in [&settings.processed_directory, &settings.error_directory]
            .into_iter()
            .flatten()
        {
            tokio::fs::create_dir_all(directory).await.map_err(|e| {
                ConnectorError(format!("cannot create {}: {e}", directory.display()))
            })?;
        }
        info!(channel = %context.channel(), directory = %settings.directory.display(), "watching directory");
        let mut seen: HashMap<PathBuf, (u64, Option<SystemTime>)> = HashMap::new();
        let mut rejected = HashSet::new();
        loop {
            match scan(&settings.directory, &settings.pattern).await {
                Ok(entries) => {
                    let mut stable: Vec<Entry> = entries
                        .iter()
                        .filter(|entry| {
                            seen.get(&entry.path) == Some(&(entry.size, entry.modified))
                        })
                        .cloned()
                        .collect();
                    seen = entries
                        .into_iter()
                        .map(|entry| (entry.path, (entry.size, entry.modified)))
                        .collect();
                    match settings.sort {
                        SortOrder::Name => stable.sort_by(|a, b| a.name.cmp(&b.name)),
                        SortOrder::Modified => {
                            stable.sort_by(|a, b| {
                                a.modified.cmp(&b.modified).then(a.name.cmp(&b.name))
                            });
                        }
                    }
                    for entry in &stable {
                        if context.is_cancelled() {
                            return Ok(());
                        }
                        self.process(&context, entry, &mut rejected).await;
                        seen.remove(&entry.path);
                    }
                    rejected.retain(|path| seen.contains_key(path));
                }
                Err(e) => {
                    warn!(channel = %context.channel(), directory = %settings.directory.display(), error = %e, "cannot scan directory")
                }
            }
            tokio::select! {
                () = context.cancelled() => return Ok(()),
                () = tokio::time::sleep(settings.poll_interval.0) => {}
            }
        }
    }
}

/// Settings of the `file` destination.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileDestinationSettings {
    /// Directory to write to.
    pub directory: PathBuf,
    /// File name template.
    #[serde(default = "default_filename")]
    pub filename: String,
    /// Replace existing files.
    #[serde(default)]
    pub overwrite: bool,
    /// Create the directory when missing.
    #[serde(default = "yes")]
    pub create_directory: bool,
}

/// Writes each delivery to a file.
#[derive(Debug, Clone)]
pub struct FileDestination {
    settings: FileDestinationSettings,
}

/// The usual file extension of a data type.
pub(crate) fn extension(data_type: Option<DataType>) -> &'static str {
    match data_type {
        Some(DataType::Hl7V2) => "hl7",
        Some(DataType::Astm) => "astm",
        Some(DataType::Poct1a | DataType::Xml | DataType::Cda) => "xml",
        Some(DataType::Json | DataType::Fhir) => "json",
        Some(DataType::Delimited) => "csv",
        Some(DataType::FixedWidth | DataType::Ncpdp) => "txt",
        Some(DataType::Dicom) => "dcm",
        Some(DataType::X12) => "x12",
        Some(DataType::Raw) | None => "bin",
    }
}

fn compact_timestamp(id: MessageId) -> String {
    let millis = i64::try_from(id.timestamp_ms()).unwrap_or_default();
    Timestamp::from_unix_millis(millis)
        .and_then(|ts| ClinicalDateTime::from_timestamp(ts, 0))
        .map(|value| {
            format!(
                "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
                value.year(),
                value.month().unwrap_or(1),
                value.day().unwrap_or(1),
                value.hour().unwrap_or(0),
                value.minute().unwrap_or(0),
                value.second().unwrap_or(0)
            )
        })
        .unwrap_or_else(|| "19700101T000000Z".to_owned())
}

/// Renders a file name template.
pub(crate) fn render(
    template: &str,
    channel: &str,
    destination: &str,
    id: MessageId,
    data_type: Option<DataType>,
) -> Result<String, String> {
    let mut out = String::with_capacity(template.len() + 32);
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let Some(close) = rest[open..].find('}') else {
            return Err(format!("unclosed placeholder in {template:?}"));
        };
        let value = match &rest[open + 1..open + close] {
            "channel" => channel.to_owned(),
            "destination" => destination.to_owned(),
            "message_id" => id.to_string(),
            "timestamp" => compact_timestamp(id),
            "extension" => extension(data_type).to_owned(),
            other => return Err(format!("unknown placeholder {{{other}}} in {template:?}")),
        };
        out.push_str(&value);
        rest = &rest[open + close + 1..];
    }
    out.push_str(rest);
    if out.is_empty()
        || out.starts_with('.')
        || out.contains(['/', '\\', ':', '*', '?', '"', '<', '>', '|'])
        || out.chars().any(char::is_control)
    {
        return Err(format!("{out:?} is not a valid file name"));
    }
    Ok(out)
}

impl FileDestination {
    /// Validates the settings and creates the destination.
    pub fn new(settings: FileDestinationSettings) -> Result<Self, EngineError> {
        render(
            &settings.filename,
            "channel",
            "destination",
            MessageId::from_parts(0, 0),
            None,
        )
        .map_err(|e| EngineError::Config(format!("file destination filename: {e}")))?;
        Ok(Self { settings })
    }

    async fn write(&self, delivery: &Delivery) -> Result<(), SendError> {
        let settings = &self.settings;
        let name = render(
            &settings.filename,
            delivery.channel.as_str(),
            delivery.destination.as_str(),
            delivery.message_id,
            delivery.data_type,
        )
        .map_err(SendError::permanent)?;
        let directory = &settings.directory;
        if settings.create_directory {
            tokio::fs::create_dir_all(directory).await.map_err(|e| {
                SendError::temporary(format!("cannot create {}: {e}", directory.display()))
            })?;
        }
        let target = directory.join(&name);
        if !settings.overwrite && tokio::fs::try_exists(&target).await.unwrap_or(false) {
            return match tokio::fs::read(&target).await {
                Ok(existing) if existing == delivery.payload => Ok(()),
                Ok(_) => Err(SendError::permanent(format!(
                    "{} already exists with different content",
                    target.display()
                ))),
                Err(e) => Err(SendError::temporary(format!(
                    "cannot read existing {}: {e}",
                    target.display()
                ))),
            };
        }
        let temporary = directory.join(format!(".{name}.{:016x}.tmp", fastrand::u64(..)));
        let written = async {
            let mut file = tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
                .await?;
            file.write_all(&delivery.payload).await?;
            file.sync_all().await?;
            drop(file);
            tokio::fs::rename(&temporary, &target).await
        }
        .await;
        if let Err(e) = written {
            let _ = tokio::fs::remove_file(&temporary).await;
            return Err(SendError::temporary(format!(
                "cannot write {}: {e}",
                target.display()
            )));
        }
        Ok(())
    }
}

#[async_trait]
impl DestinationConnector for FileDestination {
    async fn send(&self, delivery: &Delivery) -> Result<Option<Vec<u8>>, SendError> {
        self.write(delivery).await.map(|()| None)
    }
}

/// Registers the `file` source and destination.
pub fn register(registry: &mut Registry) {
    registry
        .add_source("file", |config: &SourceConfig| {
            let settings: FileSourceSettings = settings(&config.settings, "file source")?;
            Ok(Arc::new(FileSource::new(settings)?) as Arc<dyn SourceConnector>)
        })
        .add_destination("file", |config: &DestinationConfig| {
            let settings: FileDestinationSettings = settings(&config.settings, "file destination")?;
            Ok(Arc::new(FileDestination::new(settings)?) as Arc<dyn DestinationConnector>)
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_wildcards() {
        for (pattern, name, expected) in [
            ("*", "a.hl7", true),
            ("*.hl7", "RESULT.HL7", true),
            ("*.hl7", "result.hl7.tmp", false),
            ("lab_??.txt", "lab_01.txt", true),
            ("lab_??.txt", "lab_1.txt", false),
            ("a*b*c", "axxbyyc", true),
            ("a*b*c", "axxbyy", false),
            ("", "", true),
            ("**", "x", true),
        ] {
            assert_eq!(matches(pattern, name), expected, "{pattern} {name}");
        }
    }

    #[test]
    fn renders_file_names() {
        let id = MessageId::from_parts(1_790_067_600_000, 7);
        let name = render(
            "{channel}-{message_id}.{extension}",
            "lab",
            "lis",
            id,
            Some(DataType::Hl7V2),
        )
        .unwrap();
        assert_eq!(name, format!("lab-{id}.hl7"));
        assert_eq!(
            render(
                "{destination}_{timestamp}.{extension}",
                "lab",
                "lis",
                id,
                None
            )
            .unwrap(),
            "lis_20260922T090000Z.bin"
        );
        for bad in [
            "{unknown}",
            "{channel",
            "../{message_id}",
            "a/b",
            ".hidden",
            "",
        ] {
            assert!(render(bad, "lab", "lis", id, None).is_err(), "{bad}");
        }
    }
}
