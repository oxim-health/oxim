//! Running the engine: deployment, live reload of channel files, retention
//! and shutdown.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use oxim_alert::{AlertEngine, Sources};
use oxim_core::{ChannelConfig, Engine, EngineError, EngineOptions, SystemClock};
use oxim_lab::OrderCache;
use oxim_model::{ChannelId, Timestamp};
use oxim_store::{MessageStore, PrunePolicy, SqliteStore};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use oxim_server::Services;
use oxim_server::backup::InstanceLock;
use oxim_server::history::{Change, ChannelHistory};

use crate::CliResult;
use crate::components;
use crate::settings::Settings;

/// Opens the configured message store.
fn open_store(settings: &Settings) -> CliResult<Box<dyn MessageStore>> {
    let store = &settings.store;
    match store.kind {
        crate::settings::StoreKind::Sqlite => {
            let database = settings.database_path();
            let opened = match &store.encryption_key_env {
                Some(name) => {
                    let text = std::env::var(name)
                        .map_err(|_| format!("the encryption key variable {name} is not set"))?;
                    let key = oxim_store::cipher::parse_key(&text)?;
                    SqliteStore::open_encrypted(&database, &key)
                }
                None => SqliteStore::open(&database),
            }
            .map_err(|e| format!("cannot open {}: {e}", database.display()))?;
            Ok(Box::new(opened))
        }
        crate::settings::StoreKind::Postgres => {
            let name = store
                .url_env
                .clone()
                .ok_or("store type postgres needs url_env")?;
            let url = std::env::var(&name)
                .map_err(|_| format!("the database URL variable {name} is not set"))?;
            let tls: Option<oxim_connectors::tls::ClientTlsSettings> = store
                .tls
                .clone()
                .map(serde_json::from_value)
                .transpose()
                .map_err(|e| format!("store tls: {e}"))?;
            let node = store.node_id.clone().unwrap_or_else(|| {
                std::env::var("COMPUTERNAME")
                    .or_else(|_| std::env::var("HOSTNAME"))
                    .unwrap_or_else(|_| "oxim".to_owned())
            });
            // The synchronous client must not start inside the async
            // runtime, so it connects on a plain thread.
            let connected = std::thread::spawn(move || {
                oxim_store_postgres::PostgresStore::connect(&url, tls.as_ref(), &node)
            })
            .join()
            .map_err(|_| "connecting to PostgreSQL failed")?
            .map_err(|e| format!("cannot open the PostgreSQL store: {e}"))?;
            Ok(Box::new(connected))
        }
    }
}

/// Runs the engine until `shutdown` completes.
pub(crate) async fn run(settings: Settings, shutdown: impl Future<Output = ()>) -> CliResult<()> {
    std::fs::create_dir_all(&settings.data_dir)
        .map_err(|e| format!("cannot create {}: {e}", settings.data_dir.display()))?;
    // One engine per data directory; `oxim restore` checks this lock.
    let _lock = InstanceLock::acquire(&settings.data_dir).map_err(|e| match e {
        oxim_server::backup::BackupError::Running(dir) => {
            format!("another OXIM process is running with the data directory {dir}")
        }
        other => other.to_string(),
    })?;
    let history_path = settings.history_path();
    let history = Arc::new(
        ChannelHistory::open(&history_path)
            .map_err(|e| format!("cannot open {}: {e}", history_path.display()))?,
    );
    let database = settings.database_path();
    let store = open_store(&settings)?;
    let mut options = EngineOptions::default();
    options.processing_queue = settings.engine.processing_queue;
    options.shutdown_grace = settings.engine.shutdown_grace.0;
    options.idle_poll = settings.engine.idle_poll.0;
    let components = components::build(&settings);
    let devices = components.devices.clone();
    let engine = Engine::start(store, components.registry, Arc::new(SystemClock), options).await?;
    let alerts = match AlertEngine::new(settings.alerts.clone(), engine.registry()) {
        Ok(alerts) => Arc::new(alerts),
        Err(e) => {
            engine.shutdown().await;
            return Err(e.into());
        }
    };
    info!(
        version = env!("CARGO_PKG_VERSION"),
        database = %database.display(),
        channels = %settings.channels_dir.display(),
        "OXIM started"
    );
    let services = Services::new()
        .with_alerts(alerts.clone())
        .with_devices(devices.clone())
        .with_history(history.clone());
    let web = crate::web::start(&settings, &engine, services).await;
    let web = match web {
        Ok(web) => web,
        Err(e) => {
            engine.shutdown().await;
            return Err(e);
        }
    };

    let mut watcher = ChannelWatcher::new(settings.channels_dir.clone()).with_history(history);
    watcher.sync(&engine).await;
    let reload = settings.reload.enabled.then(|| {
        let engine = engine.clone();
        let interval = settings.reload.interval.0;
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(interval).await;
                watcher.sync(&engine).await;
            }
        })
    });
    let retention = tokio::spawn(retention_loop(engine.clone(), settings.clone()));
    let scheduled_backups = tokio::spawn(crate::backups::schedule_loop(settings.clone()));
    let alerting = CancellationToken::new();
    let alert_task = tokio::spawn(
        alerts.run(
            Sources {
                engine: engine.clone(),
                devices: Some(devices),
                data_dir: settings.data_dir.clone(),
                certificates: settings
                    .server
                    .tls
                    .as_ref()
                    .map(|tls| vec![tls.cert.clone()])
                    .unwrap_or_default(),
            },
            alerting.clone(),
        ),
    );

    shutdown.await;
    info!("stopping");
    crate::web::stop(web).await;
    if let Some(reload) = reload {
        reload.abort();
    }
    retention.abort();
    scheduled_backups.abort();
    alerting.cancel();
    let _ = alert_task.await;
    engine.shutdown().await;
    info!("OXIM stopped");
    Ok(())
}

/// Resolves when the process is asked to stop: Ctrl+C everywhere, and
/// SIGTERM on Unix.
pub(crate) async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = terminate.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

async fn retention_loop(engine: Engine, settings: Settings) {
    let retention = settings.retention;
    if retention.contents_after.is_none()
        && retention.messages_after.is_none()
        && retention.orders_after.is_none()
    {
        return;
    }
    let orders = settings.orders_path();
    loop {
        let now = engine.clock().now();
        let before = |age: Duration| {
            let nanos = i64::try_from(age.as_nanos()).unwrap_or(i64::MAX);
            Timestamp::from_unix_nanos(now.unix_nanos().saturating_sub(nanos))
        };
        let policy = PrunePolicy {
            contents_before: retention.contents_after.map(|age| before(age.0)),
            messages_before: retention.messages_after.map(|age| before(age.0)),
        };
        match engine.store().run(move |store| store.prune(&policy)).await {
            Ok(report) if report.contents_pruned + report.messages_pruned > 0 => info!(
                contents = report.contents_pruned,
                messages = report.messages_pruned,
                "retention pruned completed messages"
            ),
            Ok(_) => {}
            Err(e) => error!(error = %e, "retention failed"),
        }
        // The order cache exists once a lab channel has used it.
        if let Some(age) = retention.orders_after
            && orders.exists()
        {
            let path = orders.clone();
            let cutoff = before(age.0);
            let pruned = tokio::task::spawn_blocking(move || {
                OrderCache::open(&path).and_then(|cache| cache.prune(cutoff))
            })
            .await;
            match pruned {
                Ok(Ok(0)) => {}
                Ok(Ok(count)) => info!(orders = count, "retention pruned cached lab orders"),
                Ok(Err(e)) => error!(error = %e, "order cache retention failed"),
                Err(e) => error!(error = %e, "order cache retention failed"),
            }
        }
        tokio::time::sleep(retention.interval.0).await;
    }
}

/// Keeps deployed channels in line with the files in the channel directory.
pub(crate) struct ChannelWatcher {
    directory: PathBuf,
    /// Last seen text and the channel it defined, per file.
    files: BTreeMap<PathBuf, (String, Option<ChannelId>)>,
    /// Where deployed versions are recorded.
    history: Option<Arc<ChannelHistory>>,
}

impl ChannelWatcher {
    pub(crate) fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            files: BTreeMap::new(),
            history: None,
        }
    }

    /// Records every changed or removed channel file in `history`.
    pub(crate) fn with_history(mut self, history: Arc<ChannelHistory>) -> Self {
        self.history = Some(history);
        self
    }

    fn remember(&self, engine: &Engine, channel: &ChannelId, change: Change, yaml: Option<&str>) {
        if let Some(history) = &self.history
            && let Err(e) =
                history.record(channel.as_str(), change, yaml, "file", engine.clock().now())
        {
            warn!(%channel, error = %e, "cannot record the channel version");
        }
    }

    fn channel_files(directory: &Path) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(directory) else {
            return Vec::new();
        };
        let mut paths: Vec<_> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|ext| ext == "yaml" || ext == "yml")
            })
            .collect();
        paths.sort();
        paths
    }

    /// Deploys new and changed channels and undeploys removed or disabled
    /// ones. A file that fails to parse keeps its previous deployment.
    pub(crate) async fn sync(&mut self, engine: &Engine) {
        let paths = Self::channel_files(&self.directory);
        let present: BTreeSet<PathBuf> = paths.iter().cloned().collect();

        let removed: Vec<PathBuf> = self
            .files
            .keys()
            .filter(|path| !present.contains(*path))
            .cloned()
            .collect();
        for path in removed {
            if let Some((_, Some(channel))) = self.files.remove(&path) {
                info!(%channel, file = %path.display(), "channel file removed");
                undeploy(engine, &channel).await;
                self.remember(engine, &channel, Change::Deleted, None);
            }
        }

        for path in paths {
            let Ok(text) = std::fs::read_to_string(&path) else {
                warn!(file = %path.display(), "cannot read channel file");
                continue;
            };
            if self.files.get(&path).is_some_and(|(seen, _)| *seen == text) {
                continue;
            }
            let previous = self.files.get(&path).and_then(|(_, id)| id.clone());
            let config = match ChannelConfig::from_yaml(&text) {
                Ok(config) => config,
                Err(e) => {
                    error!(file = %path.display(), error = %e, "invalid channel file; keeping the previous deployment");
                    self.files.insert(path, (text, previous));
                    continue;
                }
            };
            let taken_elsewhere = self
                .files
                .iter()
                .any(|(other, (_, id))| *other != path && id.as_ref() == Some(&config.id));
            if taken_elsewhere {
                error!(channel = %config.id, file = %path.display(), "channel identifier already used by another file");
                self.files.insert(path, (text, previous));
                continue;
            }
            if let Some(previous) = previous.as_ref().filter(|id| **id != config.id) {
                undeploy(engine, previous).await;
            }
            let id = config.id.clone();
            self.remember(engine, &id, Change::Saved, Some(&text));
            if config.enabled {
                match engine.redeploy(config).await {
                    Ok(()) => {
                        info!(channel = %id, file = %path.display(), "channel deployed from file")
                    }
                    Err(e) => {
                        error!(channel = %id, file = %path.display(), error = %e, "cannot deploy channel")
                    }
                }
            } else {
                undeploy(engine, &id).await;
                info!(channel = %id, "channel disabled");
            }
            self.files.insert(path, (text, Some(id)));
        }
    }
}

async fn undeploy(engine: &Engine, channel: &ChannelId) {
    match engine.undeploy(channel).await {
        Ok(()) | Err(EngineError::NotDeployed(_)) => {}
        Err(e) => error!(%channel, error = %e, "cannot undeploy channel"),
    }
}

#[cfg(test)]
mod tests {
    use oxim_core::{EngineOptions, Registry};
    use oxim_store::SqliteStore;

    use super::*;

    #[tokio::test]
    async fn the_watcher_records_channel_versions() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::start(
            Box::new(SqliteStore::open_in_memory().unwrap()),
            Registry::new(),
            Arc::new(SystemClock),
            EngineOptions::default(),
        )
        .await
        .unwrap();
        let history = Arc::new(ChannelHistory::open_in_memory().unwrap());
        let mut watcher =
            ChannelWatcher::new(dir.path().to_path_buf()).with_history(history.clone());
        // Disabled channels need no component types.
        let path = dir.path().join("lab.yaml");
        std::fs::write(
            &path,
            "id: lab\nenabled: false\nsource: {type: none, data_type: raw}\n",
        )
        .unwrap();
        watcher.sync(&engine).await;
        watcher.sync(&engine).await;
        std::fs::write(
            &path,
            "id: lab\nenabled: false\nsource: {type: none, data_type: hl7v2}\n",
        )
        .unwrap();
        watcher.sync(&engine).await;
        std::fs::remove_file(&path).unwrap();
        watcher.sync(&engine).await;
        let versions = history.list("lab").unwrap();
        assert_eq!(
            versions
                .iter()
                .map(|v| (v.version, v.change))
                .collect::<Vec<_>>(),
            [(3, Change::Deleted), (2, Change::Saved), (1, Change::Saved)]
        );
        assert!(versions.iter().all(|v| v.actor == "file"));
        engine.shutdown().await;
    }
}
