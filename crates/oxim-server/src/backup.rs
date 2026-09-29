//! Backups of an OXIM installation, and restoring them.
//!
//! A backup is one `.tar.gz` archive holding:
//!
//! - `databases/`: a consistent copy of every SQLite database in the data
//!   directory (`oxim.db`, `auth.db`, `orders.db`, `devices.db`,
//!   `history.db`, the DICOM index and worklist, ...), taken with
//!   `VACUUM INTO` while OXIM runs;
//! - `config/channels/`, `config/tables/`, `config/scripts/` and
//!   `config/oxim.yaml`: the configuration files;
//! - `manifest.json`: the format version, the OXIM version, the creation
//!   time and the size and SHA-256 checksum of every file.
//!
//! Restoring checks every checksum, refuses to run while an `oxim run`
//! process holds the data directory (see [`InstanceLock`]) unless forced,
//! and moves every file it replaces into a `restore-previous-*` directory
//! in the data directory first.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use oxim_model::{ClinicalDateTime, Timestamp};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The archive format version written into manifests.
pub const FORMAT: u32 = 1;

/// Errors of backups and restores.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BackupError {
    /// Reading or writing files failed.
    #[error("{context}: {source}")]
    Io {
        /// What was being done.
        context: String,
        /// The error.
        source: std::io::Error,
    },
    /// Copying a database failed.
    #[error("cannot back up {database}: {source}")]
    Database {
        /// The database file.
        database: String,
        /// The error.
        source: rusqlite::Error,
    },
    /// The archive is not a valid OXIM backup.
    #[error("invalid backup: {0}")]
    Invalid(String),
    /// OXIM is running with this data directory.
    #[error(
        "OXIM is running with the data directory {0}; stop it first (or pass --force to restore anyway)"
    )]
    Running(String),
}

fn io(context: impl Into<String>) -> impl FnOnce(std::io::Error) -> BackupError {
    let context = context.into();
    move |source| BackupError::Io { context, source }
}

/// What a backup contains and where restored files go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupSources {
    /// The data directory with the databases.
    pub data_dir: PathBuf,
    /// Channel files.
    pub channels_dir: PathBuf,
    /// Code and routing tables.
    pub tables_dir: PathBuf,
    /// Script files, if configured.
    pub scripts_dir: Option<PathBuf>,
    /// The configuration file, if known.
    pub config_file: Option<PathBuf>,
}

/// One file of a backup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestFile {
    /// Path inside the archive, with `/` separators.
    pub path: String,
    /// Size in bytes.
    pub size: u64,
    /// SHA-256 of the content, in lower-case hexadecimal.
    pub sha256: String,
}

/// The manifest of a backup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// The archive format version.
    pub format: u32,
    /// The OXIM version that wrote it.
    pub oxim_version: String,
    /// When it was written (UTC, ISO 8601).
    pub created_at: String,
    /// Every file except the manifest.
    pub files: Vec<ManifestFile>,
}

/// A backup file in a backup directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BackupInfo {
    /// The file name.
    pub name: String,
    /// Size in bytes.
    pub size: u64,
    /// When the file was last modified.
    pub modified_at: Option<Timestamp>,
}

/// What a restore did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreReport {
    /// The manifest of the restored backup.
    pub manifest: Manifest,
    /// Where the replaced files were moved, if any were replaced.
    pub previous: Option<PathBuf>,
}

/// The time as `YYYYMMDDTHHMMSSZ`.
fn compact(now: Timestamp) -> String {
    ClinicalDateTime::from_timestamp(now, 0).map_or_else(
        || "19700101T000000Z".to_owned(),
        |t| {
            format!(
                "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
                t.year(),
                t.month().unwrap_or(1),
                t.day().unwrap_or(1),
                t.hour().unwrap_or(0),
                t.minute().unwrap_or(0),
                t.second().unwrap_or(0)
            )
        },
    )
}

fn iso(now: Timestamp) -> String {
    ClinicalDateTime::from_timestamp(now, 0).map_or_else(
        || "1970-01-01T00:00:00Z".to_owned(),
        |t| {
            format!(
                "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
                t.year(),
                t.month().unwrap_or(1),
                t.day().unwrap_or(1),
                t.hour().unwrap_or(0),
                t.minute().unwrap_or(0),
                t.second().unwrap_or(0)
            )
        },
    )
}

/// The file name of a backup taken at `now`:
/// `oxim-backup-20260929T020000Z.tar.gz`.
pub fn backup_name(now: Timestamp) -> String {
    format!("oxim-backup-{}.tar.gz", compact(now))
}

/// Whether `name` is a backup file name that [`backup_name`] could have
/// produced (letters, digits, `-`, `_`, `.`; no directories).
pub fn valid_name(name: &str) -> bool {
    name.starts_with("oxim-backup-")
        && name.ends_with(".tar.gz")
        && name.len() < 128
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

fn sha256_file(path: &Path) -> Result<(u64, String), BackupError> {
    let mut file = File::open(path).map_err(io(format!("cannot read {}", path.display())))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut size = 0u64;
    loop {
        let n = file
            .read(&mut buffer)
            .map_err(io(format!("cannot read {}", path.display())))?;
        if n == 0 {
            break;
        }
        size += n as u64;
        hasher.update(&buffer[..n]);
    }
    let digest = hasher.finalize();
    Ok((size, digest.iter().map(|b| format!("{b:02x}")).collect()))
}

/// The SQLite databases of a data directory.
fn databases(data_dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(data_dir) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && path.extension().is_some_and(|ext| ext == "db"))
        .collect();
    found.sort();
    found
}

/// Regular files below `dir`, skipping hidden entries (such as the
/// `.deleted` channel archive), with paths relative to `dir`.
fn files_below(dir: &Path) -> Vec<PathBuf> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            if path.is_dir() {
                walk(root, &path, out);
            } else if path.is_file()
                && let Ok(relative) = path.strip_prefix(root)
            {
                out.push(relative.to_path_buf());
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

fn slash(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// Copies a live SQLite database consistently with `VACUUM INTO`.
fn copy_database(source: &Path, target: &Path) -> Result<(), BackupError> {
    let name = source.display().to_string();
    let error = |source| BackupError::Database {
        database: name.clone(),
        source,
    };
    let conn = rusqlite::Connection::open_with_flags(
        source,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(error)?;
    conn.busy_timeout(std::time::Duration::from_secs(30))
        .map_err(error)?;
    let target = target.to_string_lossy().into_owned();
    conn.execute("VACUUM INTO ?1", [target]).map_err(error)?;
    Ok(())
}

/// A scratch directory next to `near`, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(near: &Path, now: Timestamp) -> Result<Self, BackupError> {
        let dir = near.join(format!(".oxim-scratch-{}", now.unix_nanos()));
        std::fs::create_dir_all(&dir).map_err(io(format!("cannot create {}", dir.display())))?;
        Ok(Self(dir))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Writes a backup of `sources` to `out` and returns its manifest.
pub fn create(
    sources: &BackupSources,
    out: &Path,
    now: Timestamp,
) -> Result<Manifest, BackupError> {
    let parent = out
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    std::fs::create_dir_all(&parent).map_err(io(format!("cannot create {}", parent.display())))?;
    let scratch = Scratch::new(&parent, now)?;
    let mut entries: Vec<(String, PathBuf)> = Vec::new();

    let db_dir = scratch.0.join("databases");
    std::fs::create_dir_all(&db_dir).map_err(io("cannot create the scratch directory"))?;
    for database in databases(&sources.data_dir) {
        let Some(name) = database.file_name() else {
            continue;
        };
        let copy = db_dir.join(name);
        copy_database(&database, &copy)?;
        entries.push((format!("databases/{}", name.to_string_lossy()), copy));
    }
    let mut config_dirs = vec![
        ("channels", sources.channels_dir.clone()),
        ("tables", sources.tables_dir.clone()),
    ];
    if let Some(scripts) = &sources.scripts_dir {
        config_dirs.push(("scripts", scripts.clone()));
    }
    for (label, dir) in &config_dirs {
        for relative in files_below(dir) {
            entries.push((
                format!("config/{label}/{}", slash(&relative)),
                dir.join(&relative),
            ));
        }
    }
    if let Some(file) = sources.config_file.as_ref().filter(|f| f.is_file()) {
        entries.push(("config/oxim.yaml".to_owned(), file.clone()));
    }

    let mut files = Vec::with_capacity(entries.len());
    for (path, source) in &entries {
        let (size, sha256) = sha256_file(source)?;
        files.push(ManifestFile {
            path: path.clone(),
            size,
            sha256,
        });
    }
    let manifest = Manifest {
        format: FORMAT,
        oxim_version: env!("CARGO_PKG_VERSION").to_owned(),
        created_at: iso(now),
        files,
    };
    let manifest_json =
        serde_json::to_vec_pretty(&manifest).map_err(|e| BackupError::Invalid(e.to_string()))?;

    let partial = parent.join(format!(
        ".{}.partial",
        out.file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
    ));
    let file =
        File::create(&partial).map_err(io(format!("cannot create {}", partial.display())))?;
    let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut archive = tar::Builder::new(encoder);
    let mut header = tar::Header::new_gnu();
    header.set_size(manifest_json.len() as u64);
    header.set_mode(0o600);
    header.set_mtime(u64::try_from(now.unix_millis() / 1000).unwrap_or(0));
    header.set_cksum();
    archive
        .append_data(&mut header, "manifest.json", manifest_json.as_slice())
        .map_err(io("cannot write the backup"))?;
    for (path, source) in &entries {
        let mut file =
            File::open(source).map_err(io(format!("cannot read {}", source.display())))?;
        archive
            .append_file(path, &mut file)
            .map_err(io("cannot write the backup"))?;
    }
    let encoder = archive
        .into_inner()
        .map_err(io("cannot write the backup"))?;
    let file = encoder.finish().map_err(io("cannot write the backup"))?;
    file.sync_all().map_err(io("cannot write the backup"))?;
    drop(file);
    std::fs::rename(&partial, out).map_err(io(format!("cannot write {}", out.display())))?;
    Ok(manifest)
}

/// Whether an archive path is a plain relative path without `..`.
fn safe_relative(path: &str) -> bool {
    !path.is_empty()
        && Path::new(path)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}

/// Extracts `archive` into `into` and checks it against its manifest.
fn extract(archive: &Path, into: &Path) -> Result<Manifest, BackupError> {
    let file = File::open(archive).map_err(io(format!("cannot read {}", archive.display())))?;
    let mut reader = tar::Archive::new(flate2::read::GzDecoder::new(file));
    let mut manifest: Option<Manifest> = None;
    let entries = reader.entries().map_err(io("cannot read the backup"))?;
    for entry in entries {
        let mut entry = entry.map_err(io("cannot read the backup"))?;
        let path = entry
            .path()
            .map_err(io("cannot read the backup"))?
            .to_string_lossy()
            .replace('\\', "/");
        if !safe_relative(&path) {
            return Err(BackupError::Invalid(format!("unsafe path {path:?}")));
        }
        if !entry.header().entry_type().is_file() {
            continue;
        }
        if path == "manifest.json" {
            let mut text = Vec::new();
            entry
                .read_to_end(&mut text)
                .map_err(io("cannot read the manifest"))?;
            manifest = Some(
                serde_json::from_slice(&text)
                    .map_err(|e| BackupError::Invalid(format!("manifest: {e}")))?,
            );
            continue;
        }
        let target = into.join(&path);
        if let Some(dir) = target.parent() {
            std::fs::create_dir_all(dir).map_err(io("cannot extract the backup"))?;
        }
        let mut out = File::create(&target).map_err(io("cannot extract the backup"))?;
        std::io::copy(&mut entry, &mut out).map_err(io("cannot extract the backup"))?;
        out.flush().map_err(io("cannot extract the backup"))?;
    }
    let manifest = manifest.ok_or_else(|| BackupError::Invalid("no manifest.json".into()))?;
    if manifest.format != FORMAT {
        return Err(BackupError::Invalid(format!(
            "format {} is not supported",
            manifest.format
        )));
    }
    for file in &manifest.files {
        if !safe_relative(&file.path) {
            return Err(BackupError::Invalid(format!("unsafe path {:?}", file.path)));
        }
        let path = into.join(&file.path);
        if !path.is_file() {
            return Err(BackupError::Invalid(format!("{} is missing", file.path)));
        }
        let (size, sha256) = sha256_file(&path)?;
        if size != file.size || sha256 != file.sha256 {
            return Err(BackupError::Invalid(format!(
                "{} does not match its checksum",
                file.path
            )));
        }
    }
    Ok(manifest)
}

/// Reads and checks a backup without restoring it.
pub fn verify(archive: &Path, now: Timestamp) -> Result<Manifest, BackupError> {
    let near = archive
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let scratch = Scratch::new(&near, now)?;
    extract(archive, &scratch.0)
}

/// Moves `path` (and SQLite's `-wal`/`-shm` companions of a database) into
/// `previous`, keeping its name under `label`.
fn set_aside(
    path: &Path,
    previous: &Path,
    label: &str,
    moved: &mut bool,
) -> Result<(), BackupError> {
    let mut candidates = vec![path.to_path_buf()];
    if path.extension().is_some_and(|ext| ext == "db") {
        for suffix in ["-wal", "-shm"] {
            let mut name = path.as_os_str().to_owned();
            name.push(suffix);
            candidates.push(PathBuf::from(name));
        }
    }
    for candidate in candidates {
        if !candidate.exists() {
            continue;
        }
        let Some(name) = candidate.file_name() else {
            continue;
        };
        let dir = previous.join(label);
        std::fs::create_dir_all(&dir).map_err(io(format!("cannot create {}", dir.display())))?;
        std::fs::rename(&candidate, dir.join(name))
            .map_err(io(format!("cannot move {} aside", candidate.display())))?;
        *moved = true;
    }
    Ok(())
}

/// Restores `archive` into the locations of `sources`.
pub fn restore(
    archive: &Path,
    sources: &BackupSources,
    force: bool,
    now: Timestamp,
) -> Result<RestoreReport, BackupError> {
    std::fs::create_dir_all(&sources.data_dir)
        .map_err(io(format!("cannot create {}", sources.data_dir.display())))?;
    let _lock = match InstanceLock::acquire(&sources.data_dir) {
        Ok(lock) => Some(lock),
        Err(BackupError::Running(dir)) if !force => return Err(BackupError::Running(dir)),
        Err(BackupError::Running(_)) => None,
        Err(other) => return Err(other),
    };
    let scratch = Scratch::new(&sources.data_dir, now)?;
    let manifest = extract(archive, &scratch.0)?;
    let previous = sources
        .data_dir
        .join(format!("restore-previous-{}", compact(now)));
    let mut moved = false;
    for file in &manifest.files {
        let source = scratch.0.join(&file.path);
        let target = if let Some(name) = file.path.strip_prefix("databases/") {
            sources.data_dir.join(name)
        } else if let Some(rest) = file.path.strip_prefix("config/channels/") {
            sources.channels_dir.join(rest)
        } else if let Some(rest) = file.path.strip_prefix("config/tables/") {
            sources.tables_dir.join(rest)
        } else if let Some(rest) = file.path.strip_prefix("config/scripts/") {
            match &sources.scripts_dir {
                Some(dir) => dir.join(rest),
                None => continue,
            }
        } else if file.path == "config/oxim.yaml" {
            match &sources.config_file {
                Some(path) => path.clone(),
                None => continue,
            }
        } else {
            continue;
        };
        // Replaced files keep the archive's layout, without `config/`.
        let parent = Path::new(&file.path)
            .parent()
            .map(slash)
            .unwrap_or_default();
        let label = parent
            .strip_prefix("config/")
            .map_or_else(|| parent.clone(), str::to_owned);
        set_aside(&target, &previous, &label, &mut moved)?;
        if let Some(dir) = target.parent() {
            std::fs::create_dir_all(dir).map_err(io(format!("cannot create {}", dir.display())))?;
        }
        std::fs::copy(&source, &target)
            .map_err(io(format!("cannot write {}", target.display())))?;
    }
    Ok(RestoreReport {
        manifest,
        previous: moved.then_some(previous),
    })
}

/// The backups in `dir`, newest first.
pub fn list(dir: &Path) -> Vec<BackupInfo> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut backups: Vec<BackupInfo> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let metadata = entry.metadata().ok()?;
            (metadata.is_file() && valid_name(&name)).then(|| BackupInfo {
                name,
                size: metadata.len(),
                modified_at: metadata
                    .modified()
                    .ok()
                    .and_then(Timestamp::from_system_time),
            })
        })
        .collect();
    // Names sort by time.
    backups.sort_by(|a, b| b.name.cmp(&a.name));
    backups
}

/// Deletes all but the newest `keep` backups in `dir`. Returns how many
/// were deleted.
pub fn prune(dir: &Path, keep: usize) -> Result<usize, BackupError> {
    let mut deleted = 0;
    for backup in list(dir).into_iter().skip(keep) {
        let path = dir.join(&backup.name);
        std::fs::remove_file(&path).map_err(io(format!("cannot delete {}", path.display())))?;
        deleted += 1;
    }
    Ok(deleted)
}

/// An exclusive lock on a data directory, held by `oxim run` so a restore
/// does not replace databases under a running engine.
#[derive(Debug)]
pub struct InstanceLock {
    _file: File,
}

impl InstanceLock {
    /// The lock file of a data directory.
    pub fn path(data_dir: &Path) -> PathBuf {
        data_dir.join("oxim.lock")
    }

    /// Takes the lock, or reports [`BackupError::Running`] when another
    /// process holds it.
    pub fn acquire(data_dir: &Path) -> Result<Self, BackupError> {
        let path = Self::path(data_dir);
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(io(format!("cannot open {}", path.display())))?;
        match file.try_lock() {
            Ok(()) => Ok(Self { _file: file }),
            Err(std::fs::TryLockError::WouldBlock) => {
                Err(BackupError::Running(data_dir.display().to_string()))
            }
            Err(std::fs::TryLockError::Error(e)) => {
                Err(io(format!("cannot lock {}", path.display()))(e))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: i64) -> Timestamp {
        Timestamp::from_unix_nanos(seconds * 1_000_000_000)
    }

    fn installation(root: &Path) -> BackupSources {
        let sources = BackupSources {
            data_dir: root.join("data"),
            channels_dir: root.join("channels"),
            tables_dir: root.join("tables"),
            scripts_dir: Some(root.join("scripts")),
            config_file: Some(root.join("oxim.yaml")),
        };
        for dir in [
            &sources.data_dir,
            &sources.channels_dir,
            &sources.tables_dir,
        ] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::create_dir_all(root.join("scripts/lib")).unwrap();
        let conn = rusqlite::Connection::open(sources.data_dir.join("oxim.db")).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; CREATE TABLE t (v TEXT); INSERT INTO t VALUES ('synthetic');",
        )
        .unwrap();
        drop(conn);
        std::fs::write(sources.channels_dir.join("lab.yaml"), "id: lab\n").unwrap();
        std::fs::create_dir_all(sources.channels_dir.join(".deleted")).unwrap();
        std::fs::write(
            sources.channels_dir.join(".deleted/old.yaml.1"),
            "id: old\n",
        )
        .unwrap();
        std::fs::write(sources.tables_dir.join("codes.csv"), "from,to\nGLU,1520\n").unwrap();
        std::fs::write(root.join("scripts/lib/util.js"), "return true;\n").unwrap();
        std::fs::write(root.join("oxim.yaml"), "data_dir: data\n").unwrap();
        sources
    }

    #[test]
    fn backs_up_and_restores_an_installation() {
        let dir = tempfile::tempdir().unwrap();
        let sources = installation(dir.path());
        let out = dir
            .path()
            .join("backups")
            .join(backup_name(at(1_790_000_000)));
        let name = out.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name.starts_with("oxim-backup-2026") && name.ends_with("Z.tar.gz"),
            "{name}"
        );
        assert!(valid_name(&name));
        let manifest = create(&sources, &out, at(1_790_000_000)).unwrap();
        let paths: Vec<&str> = manifest.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "databases/oxim.db",
                "config/channels/lab.yaml",
                "config/tables/codes.csv",
                "config/scripts/lib/util.js",
                "config/oxim.yaml"
            ]
        );
        assert_eq!(verify(&out, at(2)).unwrap(), manifest);
        assert_eq!(list(out.parent().unwrap()).len(), 1);

        // Change everything, then restore.
        std::fs::write(
            sources.channels_dir.join("lab.yaml"),
            "id: lab\nenabled: false\n",
        )
        .unwrap();
        std::fs::write(sources.tables_dir.join("codes.csv"), "from,to\n").unwrap();
        let conn = rusqlite::Connection::open(sources.data_dir.join("oxim.db")).unwrap();
        conn.execute("DELETE FROM t", []).unwrap();
        drop(conn);

        // A running engine holds the lock.
        let running = InstanceLock::acquire(&sources.data_dir).unwrap();
        assert!(matches!(
            restore(&out, &sources, false, at(3)),
            Err(BackupError::Running(_))
        ));
        drop(running);

        let report = restore(&out, &sources, false, at(4)).unwrap();
        let previous = report.previous.unwrap();
        assert_eq!(
            std::fs::read_to_string(sources.channels_dir.join("lab.yaml")).unwrap(),
            "id: lab\n"
        );
        assert_eq!(
            std::fs::read_to_string(previous.join("channels/lab.yaml")).unwrap(),
            "id: lab\nenabled: false\n"
        );
        assert!(previous.join("databases/oxim.db").exists());
        let conn = rusqlite::Connection::open(sources.data_dir.join("oxim.db")).unwrap();
        let value: String = conn.query_row("SELECT v FROM t", [], |r| r.get(0)).unwrap();
        assert_eq!(value, "synthetic");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("scripts/lib/util.js")).unwrap(),
            "return true;\n"
        );
    }

    #[test]
    fn rejects_tampered_archives_and_keeps_the_newest() {
        let dir = tempfile::tempdir().unwrap();
        let sources = installation(dir.path());
        let backups = dir.path().join("backups");
        for second in [10, 20, 30] {
            create(&sources, &backups.join(backup_name(at(second))), at(second)).unwrap();
        }
        assert_eq!(prune(&backups, 2).unwrap(), 1);
        let left: Vec<String> = list(&backups).into_iter().map(|b| b.name).collect();
        assert_eq!(
            left,
            [
                "oxim-backup-19700101T000030Z.tar.gz",
                "oxim-backup-19700101T000020Z.tar.gz"
            ]
        );
        let garbage = backups.join("oxim-backup-19700101T000040Z.tar.gz");
        std::fs::write(&garbage, b"not an archive").unwrap();
        assert!(verify(&garbage, at(5)).is_err());
        assert!(valid_name("oxim-backup-20260929T020000Z.tar.gz"));
        for bad in [
            "../oxim-backup-x.tar.gz",
            "backup.tar.gz",
            "oxim-backup-a/b.tar.gz",
        ] {
            assert!(!valid_name(bad), "{bad}");
        }
        assert!(!safe_relative("../x"));
        assert!(!safe_relative("/etc/passwd"));
        assert!(safe_relative("config/channels/lab.yaml"));
    }
}
