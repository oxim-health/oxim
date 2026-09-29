//! Safe file handling for channel files and code tables.

use std::io::Write;
use std::path::{Path, PathBuf};

use oxim_core::ChannelConfig;

/// Writes `content` to `path` atomically: a temporary file in the same
/// directory is written, flushed to disk and renamed over the target.
pub(crate) fn atomic_write(path: &Path, content: &[u8]) -> std::io::Result<()> {
    let directory = path
        .parent()
        .ok_or_else(|| std::io::Error::other("the target has no directory"))?;
    std::fs::create_dir_all(directory)?;
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temporary = directory.join(format!(".{name}.{}.tmp", std::process::id()));
    {
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(content)?;
        file.sync_all()?;
    }
    std::fs::rename(&temporary, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&temporary);
    })
}

/// Whether `name` is a plain file name with one of `extensions`: letters,
/// digits, `.`, `_` and `-`, not starting with a dot.
pub(crate) fn safe_file_name(name: &str, extensions: &[&str]) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        && extensions
            .iter()
            .any(|extension| name.ends_with(&format!(".{extension}")))
}

/// A channel file and what parsing it produced.
pub(crate) struct ChannelFile {
    pub(crate) path: PathBuf,
    pub(crate) text: String,
    pub(crate) parsed: Result<ChannelConfig, String>,
}

/// Every channel file in `dir`, sorted by name.
pub(crate) fn channel_files(dir: &Path) -> Vec<ChannelFile> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .is_some_and(|ext| ext == "yaml" || ext == "yml")
        })
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let parsed = ChannelConfig::from_yaml(&text).map_err(|e| e.to_string());
            ChannelFile { path, text, parsed }
        })
        .collect()
}

/// The file defining channel `id`, if any.
pub(crate) fn find_channel(dir: &Path, id: &str) -> Option<ChannelFile> {
    channel_files(dir).into_iter().find(|file| {
        file.parsed
            .as_ref()
            .is_ok_and(|config| config.id.as_str() == id)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_file_names() {
        assert!(safe_file_name("chemistry.csv", &["csv"]));
        assert!(!safe_file_name("../etc.csv", &["csv"]));
        assert!(!safe_file_name("a/b.csv", &["csv"]));
        assert!(!safe_file_name(".hidden.csv", &["csv"]));
        assert!(!safe_file_name("table.txt", &["csv"]));
        assert!(!safe_file_name("", &["csv"]));
    }

    #[test]
    fn writes_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.yaml");
        atomic_write(&path, b"one").unwrap();
        atomic_write(&path, b"two").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
