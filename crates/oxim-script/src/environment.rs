//! Where script steps find script files, and the maps they share.

use std::collections::{BTreeMap, HashMap};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use oxim_core::EngineError;

/// The largest script file OXIM reads.
const MAX_SCRIPT_LEN: u64 = 4 * 1024 * 1024;

/// Maps that outlive a message: the Mirth Connect `globalMap` (shared by
/// every script) and `globalChannelMap` (shared by the scripts of one
/// channel). Values are JSON text.
#[derive(Debug, Default)]
pub(crate) struct GlobalMaps {
    global: Mutex<BTreeMap<String, String>>,
    channels: Mutex<HashMap<String, BTreeMap<String, String>>>,
}

impl GlobalMaps {
    /// Runs `f` on the map of `scope` (`global`, or `channel` for
    /// `channel`).
    pub(crate) fn with<R>(
        &self,
        scope: &str,
        channel: &str,
        f: impl FnOnce(&mut BTreeMap<String, String>) -> R,
    ) -> Result<R, String> {
        match scope {
            "global" => {
                let mut map = self
                    .global
                    .lock()
                    .map_err(|_| "the global map is unavailable".to_owned())?;
                Ok(f(&mut map))
            }
            "channel" => {
                let mut channels = self
                    .channels
                    .lock()
                    .map_err(|_| "the channel map is unavailable".to_owned())?;
                Ok(f(channels.entry(channel.to_owned()).or_default()))
            }
            other => Err(format!("unknown map scope {other:?}")),
        }
    }
}

/// Resolves script files and holds the maps scripts share.
///
/// Script names in step settings (`file`) are paths relative to the scripts
/// directory; absolute paths and `..` are rejected so a channel cannot read
/// arbitrary files. Files are read when a channel is deployed; a running
/// channel keeps the version it was deployed with.
///
/// Scripts registered with [`ScriptEnvironment::with_script`] take
/// precedence over files, which is convenient for tests and embedded use.
#[derive(Debug, Clone)]
pub struct ScriptEnvironment {
    scripts_dir: Option<PathBuf>,
    named: Arc<HashMap<String, Arc<str>>>,
    pub(crate) globals: Arc<GlobalMaps>,
}

impl ScriptEnvironment {
    /// Scripts are read from files below `scripts_dir`.
    pub fn new(scripts_dir: impl Into<PathBuf>) -> Self {
        Self {
            scripts_dir: Some(scripts_dir.into()),
            named: Arc::new(HashMap::new()),
            globals: Arc::new(GlobalMaps::default()),
        }
    }

    /// No file access: only scripts added with
    /// [`ScriptEnvironment::with_script`] are available.
    pub fn in_memory() -> Self {
        Self {
            scripts_dir: None,
            named: Arc::new(HashMap::new()),
            globals: Arc::new(GlobalMaps::default()),
        }
    }

    /// Makes `source` available under `name`.
    pub fn with_script(mut self, name: &str, source: &str) -> Self {
        Arc::make_mut(&mut self.named).insert(name.to_owned(), Arc::from(source));
        self
    }

    /// The source of the script file `name`.
    pub(crate) fn load(&self, name: &str) -> Result<Arc<str>, EngineError> {
        if let Some(source) = self.named.get(name) {
            return Ok(source.clone());
        }
        let error = |message: String| EngineError::Config(format!("script {name:?}: {message}"));
        let base = self
            .scripts_dir
            .as_ref()
            .ok_or_else(|| error("no such script".into()))?;
        let relative = Path::new(name);
        if relative.is_absolute()
            || relative
                .components()
                .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
        {
            return Err(error(
                "must be a path below the scripts directory, without \"..\"".into(),
            ));
        }
        let path = base.join(relative);
        let len = std::fs::metadata(&path)
            .map_err(|e| error(format!("cannot read {}: {e}", path.display())))?
            .len();
        if len > MAX_SCRIPT_LEN {
            return Err(error(format!(
                "{} is larger than {MAX_SCRIPT_LEN} bytes",
                path.display()
            )));
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| error(format!("cannot read {}: {e}", path.display())))?;
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
        Ok(Arc::from(text))
    }
}
