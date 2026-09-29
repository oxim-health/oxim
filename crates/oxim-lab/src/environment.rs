//! Where lab steps find the order cache and routing tables.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use oxim_core::EngineError;

use crate::cache::{CacheError, CacheResult, OrderCache};
use crate::routing::Routing;

/// The order cache and routing tables shared by the lab steps of every
/// channel.
///
/// The cache database is opened when a step first uses it, so validating
/// channel files does not create it. Routing table names are paths relative
/// to the tables directory; absolute paths and `..` are rejected. Tables
/// are read when a channel is deployed.
#[derive(Debug, Clone)]
pub struct LabEnvironment {
    orders_db: Option<PathBuf>,
    tables_dir: Option<PathBuf>,
    cache: Arc<Mutex<Option<Arc<OrderCache>>>>,
    routings: Arc<HashMap<String, Arc<Routing>>>,
}

impl LabEnvironment {
    /// Orders are cached in the SQLite database `orders_db`; routing tables
    /// are read below `tables_dir`.
    pub fn new(orders_db: impl Into<PathBuf>, tables_dir: impl Into<PathBuf>) -> Self {
        Self {
            orders_db: Some(orders_db.into()),
            tables_dir: Some(tables_dir.into()),
            cache: Arc::new(Mutex::new(None)),
            routings: Arc::new(HashMap::new()),
        }
    }

    /// Uses an open cache and no files: only routing tables added with
    /// [`LabEnvironment::with_routing`] are available.
    pub fn with_cache(cache: Arc<OrderCache>) -> Self {
        Self {
            orders_db: None,
            tables_dir: None,
            cache: Arc::new(Mutex::new(Some(cache))),
            routings: Arc::new(HashMap::new()),
        }
    }

    /// Makes `routing` available under `name`.
    pub fn with_routing(mut self, name: &str, routing: Routing) -> Self {
        Arc::make_mut(&mut self.routings).insert(name.to_owned(), Arc::new(routing));
        self
    }

    /// The order cache, opened on first use. A failed open is retried on
    /// the next use.
    pub fn cache(&self) -> CacheResult<Arc<OrderCache>> {
        let mut slot = self.cache.lock().map_err(|_| CacheError::Unavailable)?;
        if let Some(cache) = slot.as_ref() {
            return Ok(cache.clone());
        }
        let path = self.orders_db.as_ref().ok_or(CacheError::Unavailable)?;
        let cache = Arc::new(OrderCache::open(path)?);
        *slot = Some(cache.clone());
        Ok(cache)
    }

    /// The routing table called `name`.
    pub fn routing(&self, name: &str) -> Result<Arc<Routing>, EngineError> {
        if let Some(routing) = self.routings.get(name) {
            return Ok(routing.clone());
        }
        let error =
            |message: String| EngineError::Config(format!("routing table {name:?}: {message}"));
        let base = self
            .tables_dir
            .as_ref()
            .ok_or_else(|| error("no such table".into()))?;
        let relative = Path::new(name);
        if relative.is_absolute()
            || relative
                .components()
                .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
        {
            return Err(error(
                "must be a path below the tables directory, without \"..\"".into(),
            ));
        }
        let path = base.join(relative);
        let text = std::fs::read_to_string(&path)
            .map_err(|e| error(format!("cannot read {}: {e}", path.display())))?;
        Routing::from_csv(&text)
            .map(Arc::new)
            .map_err(|e| error(e.to_string()))
    }
}
