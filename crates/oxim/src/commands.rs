//! Offline commands: validation, channel listing and message inspection.
//!
//! These commands open the database directly. They are safe to use while
//! the engine runs: SQLite serializes writers, and the engine picks up
//! requeued deliveries on its next queue check.

use std::io::Write;

use oxim_core::ChannelConfig;
use oxim_model::{ChannelId, ConnectorId, DestinationStatus, MessageId, MessageStatus};
use oxim_store::{MessageQuery, MessageStore, SqliteStore, Stage};

use crate::CliResult;
use crate::components;
use crate::settings::Settings;

/// Checks the configuration and compiles every channel file. Returns the
/// number of invalid channels.
pub(crate) fn validate(settings: &Settings, out: &mut impl Write) -> CliResult<usize> {
    let registry = components::registry(settings);
    let mut invalid = 0;
    let mut paths: Vec<_> = std::fs::read_dir(&settings.channels_dir)
        .map_err(|e| format!("cannot read {}: {e}", settings.channels_dir.display()))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext == "yaml" || ext == "yml")
        })
        .collect();
    paths.sort();
    let mut ids = std::collections::BTreeSet::new();
    for path in &paths {
        let name = path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let result = std::fs::read_to_string(path)
            .map_err(|e| e.to_string())
            .and_then(|text| ChannelConfig::from_yaml(&text).map_err(|e| e.to_string()))
            .and_then(|config| {
                if !ids.insert(config.id.clone()) {
                    return Err(format!(
                        "channel {} is defined in another file too",
                        config.id
                    ));
                }
                registry.compile(&config).map_err(|e| e.to_string())?;
                registry.source(&config.source).map_err(|e| e.to_string())?;
                for destination in &config.destinations {
                    registry
                        .destination(destination)
                        .map_err(|e| e.to_string())?;
                }
                Ok(config)
            });
        match result {
            Ok(config) => {
                let state = if config.enabled {
                    "ok"
                } else {
                    "ok (disabled)"
                };
                writeln!(out, "{state:14} {name}  [{}]", config.id)?;
            }
            Err(e) => {
                invalid += 1;
                writeln!(out, "{:14} {name}  {e}", "INVALID")?;
            }
        }
    }
    if paths.is_empty() {
        writeln!(
            out,
            "no channel files in {}",
            settings.channels_dir.display()
        )?;
    }
    let alerts = &settings.alerts;
    if !alerts.rules.is_empty() || !alerts.targets.is_empty() {
        match oxim_alert::AlertEngine::new(alerts.clone(), &registry) {
            Ok(_) => writeln!(
                out,
                "{:14} alerts  [{} rules, {} targets]",
                "ok",
                alerts.rules.len(),
                alerts.targets.len()
            )?,
            Err(e) => {
                invalid += 1;
                writeln!(out, "{:14} alerts  {e}", "INVALID")?;
            }
        }
    }
    Ok(invalid)
}

/// Lists channel files with their source and destinations.
pub(crate) fn channels(settings: &Settings, out: &mut impl Write) -> CliResult<()> {
    let channels = ChannelConfig::load_dir(&settings.channels_dir)?;
    if channels.is_empty() {
        writeln!(out, "no channels in {}", settings.channels_dir.display())?;
    }
    for channel in channels {
        let destinations: Vec<String> = channel
            .destinations
            .iter()
            .map(|d| format!("{} ({})", d.id, d.kind))
            .collect();
        writeln!(
            out,
            "{:24} {:9} {} ({}) -> {}",
            channel.id.as_str(),
            if channel.enabled {
                "enabled"
            } else {
                "disabled"
            },
            channel.source.kind,
            channel.source.data_type,
            destinations.join(", ")
        )?;
    }
    Ok(())
}

fn open_store(settings: &Settings) -> CliResult<SqliteStore> {
    let path = settings.database_path();
    if !path.exists() {
        return Err(format!("no database at {}; has the engine run yet?", path.display()).into());
    }
    Ok(SqliteStore::open(&path)?)
}

/// Lists messages, newest first.
pub(crate) fn list_messages(
    settings: &Settings,
    channel: Option<String>,
    status: Option<String>,
    destination_status: Option<String>,
    limit: usize,
    out: &mut impl Write,
) -> CliResult<()> {
    let store = open_store(settings)?;
    let query = MessageQuery {
        channel: channel.map(ChannelId::new).transpose()?,
        status: status.map(|s| s.parse::<MessageStatus>()).transpose()?,
        destination_status: destination_status
            .map(|s| s.parse::<DestinationStatus>())
            .transpose()?,
        limit,
        ..MessageQuery::default()
    };
    for message in store.list_messages(&query)? {
        let destinations: Vec<String> = message
            .destinations
            .iter()
            .map(|d| format!("{}={}", d.destination, d.status))
            .collect();
        writeln!(
            out,
            "{}  {}  {:16} {:11} {}",
            message.id,
            message.received_at,
            message.channel.as_str(),
            message.status.as_str(),
            destinations.join(" ")
        )?;
    }
    Ok(())
}

/// Prints one message with its states and a stage's content.
pub(crate) fn show_message(
    settings: &Settings,
    id: &str,
    stage: &str,
    destination: Option<String>,
    out: &mut impl Write,
) -> CliResult<()> {
    let store = open_store(settings)?;
    let id: MessageId = id.parse()?;
    let record = store
        .message(id)?
        .ok_or_else(|| format!("message {id} not found"))?;
    writeln!(out, "id:           {}", record.id)?;
    writeln!(out, "channel:      {}", record.channel)?;
    writeln!(out, "source:       {}", record.connector)?;
    writeln!(out, "received:     {}", record.received_at)?;
    writeln!(out, "data type:    {}", record.data_type)?;
    writeln!(out, "status:       {}", record.status)?;
    if let Some(peer) = &record.peer {
        writeln!(out, "peer:         {peer}")?;
    }
    if let Some(error) = &record.error {
        writeln!(out, "error:        {error}")?;
    }
    for d in &record.destinations {
        write!(
            out,
            "destination:  {} {} attempts={}",
            d.destination, d.status, d.attempts
        )?;
        if let Some(error) = &d.last_error {
            write!(out, " last_error={error:?}")?;
        }
        writeln!(out)?;
    }
    let stage: Stage = stage.parse().map_err(|()| {
        format!("unknown stage {stage:?}; use raw, normalized, transformed, encoded or response")
    })?;
    let destination = destination.map(ConnectorId::new).transpose()?;
    match store.content(id, stage, destination.as_ref())? {
        Some(content) => {
            writeln!(out, "--- {stage} ---")?;
            // Segment separators are carriage returns; show them as lines.
            let text = String::from_utf8_lossy(&content.data).replace('\r', "\n");
            writeln!(out, "{}", text.trim_end())?;
        }
        None => writeln!(out, "--- no {stage} content ---")?,
    }
    Ok(())
}

/// Discards derived data and processes a message again.
pub(crate) fn reprocess(settings: &Settings, id: &str, out: &mut impl Write) -> CliResult<()> {
    let mut store = open_store(settings)?;
    let id: MessageId = id.parse()?;
    store.reprocess(id)?;
    writeln!(
        out,
        "message {id} will be processed again when its channel is (re)deployed"
    )?;
    Ok(())
}

/// Puts a failed delivery back in its queue.
pub(crate) fn requeue(
    settings: &Settings,
    id: &str,
    destination: &str,
    out: &mut impl Write,
) -> CliResult<()> {
    let mut store = open_store(settings)?;
    let id: MessageId = id.parse()?;
    let destination = ConnectorId::new(destination)?;
    let now = oxim_core::Clock::now(&oxim_core::SystemClock);
    store.requeue(id, &destination, now)?;
    writeln!(out, "delivery of {id} to {destination} queued again")?;
    Ok(())
}
