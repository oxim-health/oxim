//! Where steps find code tables.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use oxim_core::EngineError;

use crate::table::CodeTable;

#[derive(Debug, Clone)]
struct Cached {
    modified: Option<SystemTime>,
    len: u64,
    table: Arc<CodeTable>,
}

/// Resolves and caches the code tables that steps reference.
///
/// Table names in step settings are paths relative to the base directory,
/// normally the directory that holds the channel files; absolute paths and
/// `..` are rejected so a channel cannot read arbitrary files. Tables are
/// read when a channel is deployed and cached; a redeploy reloads a table
/// whose file changed (size or modification time). A running channel keeps
/// the version it was deployed with.
///
/// Tables registered with [`TransformEnvironment::with_table`] take
/// precedence over files, which is convenient for tests and embedded use.
#[derive(Debug, Clone)]
pub struct TransformEnvironment {
    base_dir: Option<PathBuf>,
    named: Arc<HashMap<String, Arc<CodeTable>>>,
    cache: Arc<Mutex<HashMap<(PathBuf, bool), Cached>>>,
}

impl TransformEnvironment {
    /// Tables are read from files below `base_dir`.
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: Some(base_dir.into()),
            named: Arc::new(HashMap::new()),
            cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// No file access: only tables added with
    /// [`TransformEnvironment::with_table`] are available.
    pub fn in_memory() -> Self {
        Self {
            base_dir: None,
            named: Arc::new(HashMap::new()),
            cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Makes `table` available under `name`.
    pub fn with_table(mut self, name: &str, table: CodeTable) -> Self {
        Arc::make_mut(&mut self.named).insert(name.to_owned(), Arc::new(table));
        self
    }

    /// The table called `name`.
    pub fn table(&self, name: &str, case_insensitive: bool) -> Result<Arc<CodeTable>, EngineError> {
        if let Some(table) = self.named.get(name) {
            return Ok(table.clone());
        }
        let error =
            |message: String| EngineError::Config(format!("code table {name:?}: {message}"));
        let base = self
            .base_dir
            .as_ref()
            .ok_or_else(|| error("no such table".into()))?;
        let relative = Path::new(name);
        if relative.is_absolute()
            || relative
                .components()
                .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
        {
            return Err(error(
                "use a path relative to the configuration directory, without ..".into(),
            ));
        }
        let path = base.join(relative);
        let metadata = std::fs::metadata(&path).map_err(|e| error(e.to_string()))?;
        let modified = metadata.modified().ok();
        let key = (path.clone(), case_insensitive);
        let mut cache = self
            .cache
            .lock()
            .map_err(|_| error("the table cache is unavailable".into()))?;
        if let Some(cached) = cache.get(&key)
            && cached.modified == modified
            && cached.len == metadata.len()
        {
            return Ok(cached.table.clone());
        }
        let text = std::fs::read_to_string(&path).map_err(|e| error(e.to_string()))?;
        let table = Arc::new(
            CodeTable::from_csv_with(&text, case_insensitive).map_err(|e| error(e.to_string()))?,
        );
        cache.insert(
            key,
            Cached {
                modified,
                len: metadata.len(),
                table: table.clone(),
            },
        );
        Ok(table)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_caches_and_reloads_tables() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("tables")).unwrap();
        let file = dir.path().join("tables").join("tests.csv");
        std::fs::write(&file, "from,to\nGLU,1520\n").unwrap();
        let environment = TransformEnvironment::new(dir.path());
        let first = environment.table("tables/tests.csv", false).unwrap();
        let again = environment.table("tables/tests.csv", false).unwrap();
        assert!(Arc::ptr_eq(&first, &again));
        std::fs::write(&file, "from,to\nGLU,1520\nHGB,2010\n").unwrap();
        let reloaded = environment.table("tables/tests.csv", false).unwrap();
        assert_eq!(reloaded.len(), 2);
        assert_eq!(first.len(), 1);
    }

    #[test]
    fn refuses_paths_outside_the_base_directory() {
        let dir = tempfile::tempdir().unwrap();
        let environment = TransformEnvironment::new(dir.path());
        for bad in [
            "../secret.csv",
            "tables/../../x.csv",
            "/etc/passwd",
            "missing.csv",
        ] {
            assert!(environment.table(bad, false).is_err(), "{bad}");
        }
        let memory = TransformEnvironment::in_memory()
            .with_table("tests", CodeTable::from_csv("from,to\nA,1\n").unwrap());
        assert_eq!(memory.table("tests", false).unwrap().len(), 1);
        assert!(memory.table("other.csv", false).is_err());
    }
}
