//! Log output.

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;

use crate::CliResult;
use crate::settings::{LogFormat, LogSettings};

/// Installs the global logger. Keep the returned guard alive until the
/// program ends so buffered log lines are written.
pub(crate) fn init(settings: &LogSettings) -> CliResult<Option<WorkerGuard>> {
    let filter = match std::env::var("OXIM_LOG") {
        Ok(filter) => EnvFilter::try_new(filter),
        Err(_) => EnvFilter::try_new(&settings.level),
    }
    .map_err(|e| format!("invalid log filter: {e}"))?;
    let builder = tracing_subscriber::fmt().with_env_filter(filter);
    let (writer, guard) = match &settings.directory {
        Some(directory) => {
            std::fs::create_dir_all(directory)
                .map_err(|e| format!("cannot create {}: {e}", directory.display()))?;
            let appender = tracing_appender::rolling::daily(directory, "oxim.log");
            let (writer, guard) = tracing_appender::non_blocking(appender);
            (Some(writer), Some(guard))
        }
        None => (None, None),
    };
    let result = match (settings.format, writer) {
        (LogFormat::Text, Some(writer)) => builder.with_ansi(false).with_writer(writer).try_init(),
        (LogFormat::Json, Some(writer)) => builder.json().with_writer(writer).try_init(),
        (LogFormat::Text, None) => builder.try_init(),
        (LogFormat::Json, None) => builder.json().try_init(),
    };
    result.map_err(|e| format!("cannot install the logger: {e}"))?;
    Ok(guard)
}
