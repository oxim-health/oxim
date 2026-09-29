//! `oxim init`: a starter configuration.

use std::io::Write;
use std::path::Path;

use crate::CliResult;

const SETTINGS: &str = "\
# OXIM engine configuration. Relative paths are resolved against this file.

# Where the message database (oxim.db) and the lab order cache (orders.db)
# are stored.
data_dir: data

# Channel files (*.yaml). Changes are picked up while OXIM runs.
channels_dir: channels

# Code and routing tables referenced by channel steps.
tables_dir: tables

log:
  level: info          # or e.g. \"info,oxim_connectors=debug\"; OXIM_LOG overrides
  format: text         # text or json
  # directory: logs    # write daily files instead of standard output

retention:
  contents_after: 90d  # delete message contents of completed messages
  messages_after: 365d # delete completed messages entirely
  orders_after: 90d    # delete cached lab orders not changed since
  interval: 1h

reload:
  enabled: true
  interval: 5s
";

const EXAMPLE_CHANNEL: &str = "\
# Example channel: receive HL7 v2 over MLLP and archive every message to
# files. Rename this file to end in .yaml to activate it.
id: example-archive
name: Example HL7 archive
enabled: true
source:
  type: mllp
  data_type: hl7v2
  settings:
    listen: 127.0.0.1:2575
destinations:
  - id: archive
    type: file
    settings:
      directory: archive
";

/// Writes `oxim.yaml`, the data, channel and table directories and an
/// inactive example channel. Existing files are kept unless `force`.
pub(crate) fn init(directory: &Path, force: bool, out: &mut impl Write) -> CliResult<()> {
    std::fs::create_dir_all(directory)?;
    for sub in ["data", "channels", "tables"] {
        std::fs::create_dir_all(directory.join(sub))?;
    }
    let files = [
        (directory.join("oxim.yaml"), SETTINGS),
        (
            directory
                .join("channels")
                .join("example-archive.yaml.example"),
            EXAMPLE_CHANNEL,
        ),
    ];
    for (path, content) in files {
        if path.exists() && !force {
            writeln!(out, "kept     {}", path.display())?;
            continue;
        }
        std::fs::write(&path, content)?;
        writeln!(out, "created  {}", path.display())?;
    }
    writeln!(
        out,
        "\nNext: add channel files to {} and start with `oxim run`.",
        directory.join("channels").display()
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;

    #[test]
    fn writes_a_loadable_configuration() {
        let dir = tempfile::tempdir().unwrap();
        let mut out = Vec::new();
        init(dir.path(), false, &mut out).unwrap();
        let settings = Settings::load(&dir.path().join("oxim.yaml")).unwrap();
        assert!(settings.channels_dir.ends_with("channels"));
        let example = std::fs::read_to_string(
            dir.path()
                .join("channels")
                .join("example-archive.yaml.example"),
        )
        .unwrap();
        oxim_core::ChannelConfig::from_yaml(&example).unwrap();
        init(dir.path(), false, &mut out).unwrap();
        assert!(String::from_utf8(out).unwrap().contains("kept"));
    }
}
