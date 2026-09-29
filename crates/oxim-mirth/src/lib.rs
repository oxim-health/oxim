//! Imports Mirth Connect channels into OXIM channel files.
//!
//! [`import`] reads a Mirth Connect 3.x or 4.x export — a single channel,
//! a channel group, a code template library or a full server configuration
//! backup — and returns one OXIM channel YAML file per channel plus a
//! [`MigrationReport`] that lists every element as converted, approximated
//! or unsupported, with the reason and its location in the export.
//!
//! ```
//! let export = r#"<channel version="3.12.0">
//!   <id>9b7c0000-0000-4000-8000-000000000001</id>
//!   <name>ADT Inbound</name>
//!   <sourceConnector>
//!     <properties class="com.mirth.connect.connectors.tcp.TcpReceiverProperties">
//!       <listenerConnectorProperties><host>0.0.0.0</host><port>6661</port></listenerConnectorProperties>
//!       <transmissionModeProperties class="com.mirth.connect.plugins.mllpmode.MLLPModeProperties">
//!         <pluginPointName>MLLP</pluginPointName>
//!       </transmissionModeProperties>
//!     </properties>
//!     <transformer><inboundDataType>HL7V2</inboundDataType></transformer>
//!   </sourceConnector>
//!   <destinationConnectors/>
//! </channel>"#;
//! let result = oxim_mirth::import(export, &oxim_mirth::ImportOptions::new())?;
//! let channel = &result.channels[0];
//! assert_eq!(channel.file_name, "adt-inbound.yaml");
//! assert!(channel.yaml.contains("type: mllp"));
//! println!("{}", result.report.to_markdown());
//! # Ok::<(), oxim_mirth::MirthError>(())
//! ```
//!
//! # What is converted
//!
//! | Mirth Connect | OXIM |
//! |---|---|
//! | TCP Listener, MLLP mode | `mllp` source (`listen`) |
//! | TCP Listener, basic TCP mode | `tcp` source with delimited or per-connection framing |
//! | TCP Sender, MLLP / basic TCP | `mllp` / `tcp` destination (`target`) |
//! | File Reader / File Writer (local files) | `file` source / destination |
//! | HTTP Sender (POST, PUT) | `http` destination |
//! | Other connectors (database, JavaScript, channel reader/writer, SMTP, JMS, DICOM, web service, document writer, FTP/SFTP/SMB/S3 files, HTTP listener) | reported unsupported; a channel whose source is unsupported is written as a `.yaml.draft` |
//! | Data types HL7V2, XML, JSON, RAW, DELIMITED (and fixed width) | `hl7v2`, `xml`, `json`, `raw`, `delimited` / `fixed_width` with `source.format` |
//! | Data types HL7V3, EDI/X12, NCPDP, DICOM | `xml`, `x12`, `ncpdp`, `dicom` (reported) |
//! | Rule builder rules on HL7 v2 fields | `path-equals`, `path-in`, `path-exists` or `condition` filters |
//! | Rules joined with OR | one `condition` filter (`any`/`all`), or one `script` filter when JavaScript is involved |
//! | JavaScript rules and steps | `script` steps with `mirth: true`, prefixed with the code template functions they call |
//! | Mapper steps (channel or connector map) | `map` `store` operations (message variables) |
//! | Message builder steps on HL7 v2 fields | `map` `copy` and `set` operations |
//! | Mapper and message builder steps that need JavaScript | `script` steps with the JavaScript Mirth generates |
//! | XSLT and other plugin steps, response transformers | reported unsupported |
//! | Queue, retry interval, retry count, rotate | destination `queue` (fixed interval, `max_attempts`, `best_effort`) |
//! | Response from a destination (`d1`) | `source.response` with `mode: destination` |
//! | Destination chains, channel and global scripts, attachments, storage settings | reported |
//!
//! `${name}` placeholders in connector settings are replaced from the
//! configuration map of a server configuration backup or from
//! [`ImportOptions::with_value`]. The `script` steps expect OXIM's script
//! runtime in Mirth compatibility mode (`mirth: true`), which offers the
//! `msg`, `tmp` and channel map conventions of Mirth scripts.

mod channel;
mod connectors;
mod context;
mod e4x;
mod error;
mod ids;
mod report;
mod steps;
mod templates;
mod xml;
mod yaml;

use std::collections::BTreeMap;

pub use error::MirthError;
pub use report::{ChannelSummary, MigrationReport, Outcome, ReportItem};

use crate::context::Notes;
use crate::ids::Names;
use crate::templates::{Globals, TemplateKind, is_trivial};
use crate::xml::Element;

/// Options of [`import`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ImportOptions {
    /// Further exports (for example a code template library export or a
    /// server configuration backup) whose code templates, global scripts
    /// and configuration map apply to the imported channels. Their channels
    /// are not imported.
    pub libraries: Vec<String>,
    /// Values for `${name}` placeholders; they take precedence over the
    /// configuration map of the export.
    pub configuration: BTreeMap<String, String>,
    /// Whether channels that are disabled in Mirth are imported (as
    /// disabled OXIM channels). When `false` they are skipped.
    pub include_disabled: bool,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            libraries: Vec::new(),
            configuration: BTreeMap::new(),
            include_disabled: true,
        }
    }
}

impl ImportOptions {
    /// Default options.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds an export whose code templates, global scripts and
    /// configuration map apply to the imported channels.
    pub fn with_library(mut self, xml: impl Into<String>) -> Self {
        self.libraries.push(xml.into());
        self
    }

    /// Supplies the value of a `${name}` placeholder.
    pub fn with_value(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.configuration.insert(name.into(), value.into());
        self
    }

    /// Skips channels that are disabled in Mirth.
    pub fn skip_disabled(mut self) -> Self {
        self.include_disabled = false;
        self
    }
}

/// One imported channel.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ImportedChannel {
    /// The OXIM channel identifier.
    pub id: String,
    /// The Mirth channel identifier.
    pub mirth_id: Option<String>,
    /// The Mirth channel name.
    pub mirth_name: String,
    /// The file name to write the channel to: `<id>.yaml`, or
    /// `<id>.yaml.draft` for a draft.
    pub file_name: String,
    /// The channel file.
    pub yaml: String,
    /// Whether the channel is enabled.
    pub enabled: bool,
    /// Whether the channel is a draft that OXIM does not load, because its
    /// source connector has no OXIM equivalent yet.
    pub draft: bool,
}

/// The result of [`import`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ImportResult {
    /// The channels, in export order.
    pub channels: Vec<ImportedChannel>,
    /// What became of every element of the export.
    pub report: MigrationReport,
}

fn export_kind(root: &Element) -> Result<String, MirthError> {
    let kind = match root.name.as_str() {
        "channel" => "channel export",
        "channelGroup" => "channel group export",
        "serverConfiguration" => "server configuration backup",
        "codeTemplateLibrary" => "code template library export",
        "list" => "export list",
        other => return Err(MirthError::NotAnExport(other.to_owned())),
    };
    let version = root.attribute("version").map(str::to_owned).or_else(|| {
        root.children
            .iter()
            .find_map(|child| child.attribute("version"))
            .map(str::to_owned)
    });
    Ok(match version {
        Some(version) => format!("{kind} (Mirth Connect {version})"),
        None => kind.to_owned(),
    })
}

/// The channel definitions in an export (channel references in groups have
/// no source connector and are skipped).
fn channels<'a>(element: &'a Element, out: &mut Vec<&'a Element>) {
    if element.name == "channel" && element.child("sourceConnector").is_some() {
        out.push(element);
        return;
    }
    for child in &element.children {
        channels(child, out);
    }
}

/// Reports the server-wide parts of an export.
fn server_items(root: &Element, globals: &Globals, notes: &mut Notes<'_>) {
    for library in &globals.libraries {
        let functions = library
            .templates
            .iter()
            .filter(|t| t.kind == TemplateKind::Function)
            .count();
        let compiled = library
            .templates
            .iter()
            .filter(|t| t.kind == TemplateKind::Compiled)
            .count();
        if functions + compiled == 0 {
            continue;
        }
        notes.converted(
            &format!("code template library \"{}\"", library.name),
            &library.location,
            format!(
                "{functions} function template(s) and {compiled} compiled code block(s); each \
                 script step includes the templates it needs"
            ),
        );
    }
    for script in &globals.scripts {
        if is_trivial(&script.body) {
            continue;
        }
        let advice = match script.name.as_str() {
            "Preprocessor" => "move the logic into transformer `script` steps of the channels",
            "Postprocessor" => "move the logic into destinations or alerts",
            _ => "run the logic outside OXIM",
        };
        notes.unsupported(
            &format!("global {} script", script.name.to_ascii_lowercase()),
            &script.location,
            format!("global scripts are not supported; {advice}"),
        );
    }
    if !globals.configuration.is_empty()
        && let Some(location) = &globals.configuration_location
    {
        notes.converted(
            "configuration map",
            location,
            format!(
                "{} value(s) are substituted where connector settings use ${{name}}",
                globals.configuration.len()
            ),
        );
    }
    for (name, what, detail) in [
        (
            "alerts",
            "alerts",
            "Mirth alerts are not imported; configure OXIM alerts",
        ),
        (
            "users",
            "users",
            "users and their passwords are not imported; create OXIM accounts",
        ),
    ] {
        if let Some(element) = root.child(name)
            && !element.children.is_empty()
        {
            notes.unsupported(
                &format!("{what} ({})", element.children.len()),
                &element.location,
                detail,
            );
        }
    }
}

/// Imports a Mirth Connect export.
pub fn import(xml: &str, options: &ImportOptions) -> Result<ImportResult, MirthError> {
    let root = xml::parse(xml)?;
    let export = export_kind(&root)?;
    let mut globals = Globals {
        overrides: options.configuration.clone(),
        ..Globals::default()
    };
    globals.collect(&root);
    for library in &options.libraries {
        let library = xml::parse(library)?;
        export_kind(&library)?;
        globals.collect(&library);
    }
    let mut items = Vec::new();
    server_items(&root, &globals, &mut Notes::new(None, &mut items, &globals));
    let mut found = Vec::new();
    channels(&root, &mut found);
    let mut ids = Names::default();
    let mut imported = Vec::new();
    for channel in found {
        if let Some(converted) = channel::convert(channel, &globals, &mut ids, options, &mut items)?
        {
            imported.push(converted);
        }
    }
    let summaries = imported
        .iter()
        .map(|channel| ChannelSummary {
            mirth_id: channel.mirth_id.clone(),
            mirth_name: channel.mirth_name.clone(),
            oxim_id: channel.id.clone(),
            file_name: channel.file_name.clone(),
            draft: channel.draft,
        })
        .collect();
    Ok(ImportResult {
        channels: imported,
        report: MigrationReport {
            export,
            channels: summaries,
            items,
        },
    })
}
