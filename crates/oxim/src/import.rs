//! `oxim import`: channels from other integration engines.

use std::io::Write;
use std::path::{Path, PathBuf};

use oxim_mirth::{ImportOptions, Outcome};

use crate::CliResult;

/// Options of `oxim import mirth`.
#[derive(Debug)]
pub(crate) struct MirthImport {
    /// The Mirth Connect export.
    pub(crate) export: PathBuf,
    /// Where the channel files go.
    pub(crate) out: PathBuf,
    /// Further exports with code templates, global scripts or configuration
    /// map values.
    pub(crate) libraries: Vec<PathBuf>,
    /// `name=value` pairs for `${name}` placeholders.
    pub(crate) values: Vec<String>,
    /// Where the report goes (`.json` for JSON, Markdown otherwise).
    pub(crate) report: Option<PathBuf>,
    /// Show what would be written without writing.
    pub(crate) dry_run: bool,
    /// Replace existing files.
    pub(crate) force: bool,
    /// Skip channels that are disabled in Mirth.
    pub(crate) skip_disabled: bool,
}

fn read(path: &Path) -> CliResult<String> {
    std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()).into())
}

/// Imports a Mirth Connect export into channel files and a report.
pub(crate) fn mirth(import: &MirthImport, out: &mut impl Write) -> CliResult<()> {
    let mut options = ImportOptions::new();
    for library in &import.libraries {
        options = options.with_library(read(library)?);
    }
    for pair in &import.values {
        let (name, value) = pair
            .split_once('=')
            .ok_or_else(|| format!("--value {pair:?}: expected name=value"))?;
        options = options.with_value(name.trim(), value);
    }
    if import.skip_disabled {
        options = options.skip_disabled();
    }
    let result = oxim_mirth::import(&read(&import.export)?, &options)?;
    let report = &result.report;
    let report_path = import
        .report
        .clone()
        .unwrap_or_else(|| import.out.join("mirth-migration-report.md"));
    let report_text = if report_path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
    {
        report.to_json()?
    } else {
        report.to_markdown()
    };

    let targets: Vec<(PathBuf, &str)> = result
        .channels
        .iter()
        .map(|channel| (import.out.join(&channel.file_name), channel.yaml.as_str()))
        .collect();
    if !import.dry_run && !import.force {
        let existing: Vec<String> = targets
            .iter()
            .map(|(path, _)| path)
            .chain(std::iter::once(&report_path))
            .filter(|path| path.exists())
            .map(|path| path.display().to_string())
            .collect();
        if !existing.is_empty() {
            return Err(format!(
                "these files exist (use --force to replace them): {}",
                existing.join(", ")
            )
            .into());
        }
    }
    let verb = if import.dry_run {
        "would write"
    } else {
        "created"
    };
    if !import.dry_run {
        std::fs::create_dir_all(&import.out)
            .map_err(|e| format!("cannot create {}: {e}", import.out.display()))?;
    }
    for (channel, (path, yaml)) in result.channels.iter().zip(&targets) {
        if !import.dry_run {
            std::fs::write(path, yaml)
                .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        }
        let note = match (channel.draft, channel.enabled) {
            (true, _) => " (draft: the source connector needs attention)",
            (false, false) => " (disabled)",
            (false, true) => "",
        };
        writeln!(out, "{verb}  {}{note}", path.display())?;
    }
    if import.dry_run {
        writeln!(out, "\n{report_text}")?;
    } else {
        if let Some(parent) = report_path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        std::fs::write(&report_path, &report_text)
            .map_err(|e| format!("cannot write {}: {e}", report_path.display()))?;
        writeln!(out, "{verb}  {}", report_path.display())?;
    }
    writeln!(
        out,
        "\n{} channel(s): {} converted, {} approximated, {} unsupported item(s).",
        result.channels.len(),
        report.count(Outcome::Converted),
        report.count(Outcome::Approximated),
        report.count(Outcome::Unsupported),
    )?;
    if report.count(Outcome::Approximated) + report.count(Outcome::Unsupported) > 0 {
        writeln!(
            out,
            "Review the report before deploying: approximated and unsupported items behave \
             differently from Mirth Connect."
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXPORT: &str = r#"<channel version="3.12.0">
      <id>c1</id><name>ADT In</name>
      <sourceConnector>
        <properties class="com.mirth.connect.connectors.tcp.TcpReceiverProperties">
          <listenerConnectorProperties><host>0.0.0.0</host><port>${port}</port></listenerConnectorProperties>
          <transmissionModeProperties><pluginPointName>MLLP</pluginPointName></transmissionModeProperties>
        </properties>
        <transformer><inboundDataType>HL7V2</inboundDataType></transformer>
      </sourceConnector>
      <destinationConnectors/>
    </channel>"#;

    fn options(dir: &Path) -> MirthImport {
        let export = dir.join("export.xml");
        std::fs::write(&export, EXPORT).unwrap();
        MirthImport {
            export,
            out: dir.join("channels"),
            libraries: Vec::new(),
            values: vec!["port=6661".into()],
            report: None,
            dry_run: false,
            force: false,
            skip_disabled: false,
        }
    }

    #[test]
    fn writes_channels_and_the_report() {
        let dir = tempfile::tempdir().unwrap();
        let mut import = options(dir.path());
        import.dry_run = true;
        let mut out = Vec::new();
        mirth(&import, &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("would write"), "{text}");
        assert!(text.contains("# Mirth Connect migration report"), "{text}");
        assert!(!dir.path().join("channels").exists());

        import.dry_run = false;
        let mut out = Vec::new();
        mirth(&import, &mut out).unwrap();
        let channel = std::fs::read_to_string(dir.path().join("channels/adt-in.yaml")).unwrap();
        assert!(channel.contains("listen: \"0.0.0.0:6661\""), "{channel}");
        assert!(
            dir.path()
                .join("channels/mirth-migration-report.md")
                .exists()
        );
        let loaded = oxim_core::ChannelConfig::load_dir(&dir.path().join("channels")).unwrap();
        assert_eq!(loaded.len(), 1);

        // Existing files are kept unless forced.
        assert!(mirth(&import, &mut Vec::new()).is_err());
        import.force = true;
        import.report = Some(dir.path().join("report.json"));
        mirth(&import, &mut Vec::new()).unwrap();
        let json = std::fs::read_to_string(dir.path().join("report.json")).unwrap();
        assert!(json.trim_start().starts_with('{'));

        import.values = vec!["port".into()];
        assert!(mirth(&import, &mut Vec::new()).is_err());
    }
}
