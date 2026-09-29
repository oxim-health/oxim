//! One Mirth channel as one OXIM channel file.

use crate::connectors::{self, Connector, duration, transport};
use crate::context::Notes;
use crate::error::MirthError;
use crate::ids::{Names, slug};
use crate::report::{Outcome, ReportItem};
use crate::steps::{self, Scope};
use crate::templates::{Globals, is_trivial};
use crate::xml::Element;
use crate::yaml::{self, Yaml};
use crate::{ImportOptions, ImportedChannel};

/// The OXIM data type (and `source.format`) of a Mirth data type.
fn data_type(transformer: Option<&Element>, notes: &mut Notes<'_>) -> (&'static str, Option<Yaml>) {
    let name = transformer
        .and_then(|t| t.value("inboundDataType"))
        .unwrap_or("HL7V2");
    let location = transformer
        .and_then(|t| t.child("inboundDataType"))
        .map_or(String::new(), |e| e.location.clone());
    let label = format!("source data type {name}");
    let unparsed = |notes: &mut Notes<'_>, oxim: &'static str| {
        notes.approximated(
            &label,
            &location,
            format!(
                "became `{oxim}`; OXIM stores these messages unparsed, so steps that read \
                 fields do not apply to them yet"
            ),
        );
        (oxim, None)
    };
    match name {
        "HL7V2" => ("hl7v2", None),
        "XML" => ("xml", None),
        "JSON" => ("json", None),
        "RAW" => ("raw", None),
        "HL7V3" => {
            notes.approximated(
                &label,
                &location,
                "HL7 v3 and CDA documents are read as generic XML (`xml`)",
            );
            ("xml", None)
        }
        "DELIMITED" => delimited(transformer, &label, &location, notes),
        "EDI" | "X12" => unparsed(notes, "x12"),
        "NCPDP" => unparsed(notes, "ncpdp"),
        "DICOM" => unparsed(notes, "dicom"),
        other => {
            notes.unsupported(
                &label,
                &location,
                format!("the data type {other} is not known; messages are kept as `raw` bytes"),
            );
            ("raw", None)
        }
    }
}

fn delimited(
    transformer: Option<&Element>,
    label: &str,
    location: &str,
    notes: &mut Notes<'_>,
) -> (&'static str, Option<Yaml>) {
    let properties = transformer.and_then(|t| t.child("inboundProperties"));
    let serialization = properties.and_then(|p| p.child("serializationProperties"));
    let value = |name: &str| serialization.and_then(|s| s.value(name));
    if let Some(widths) = value("columnWidths") {
        let names: Vec<String> = value("columnNames")
            .map(|names| names.split(',').map(|n| n.trim().to_owned()).collect())
            .unwrap_or_default();
        let mut fields = Vec::new();
        let mut start = 0i64;
        for (index, width) in widths.split(',').enumerate() {
            let Ok(width) = width.trim().parse::<i64>() else {
                notes.approximated(
                    label,
                    location,
                    format!("the column widths {widths:?} could not be read"),
                );
                return ("fixed_width", None);
            };
            let name = names
                .get(index)
                .filter(|n| !n.is_empty())
                .cloned()
                .unwrap_or_else(|| format!("column{}", index + 1));
            fields.push(Yaml::map([
                ("name", Yaml::str(name)),
                ("start", Yaml::Int(start)),
                ("length", Yaml::Int(width)),
            ]));
            start += width;
        }
        notes.converted(
            label,
            location,
            "fixed-width columns became `fixed_width` fields",
        );
        return (
            "fixed_width",
            Some(Yaml::map([("fields", Yaml::List(fields))])),
        );
    }
    let mut format = Vec::new();
    if let Some(delimiter) = value("columnDelimiter") {
        if delimiter == "\\t" || (delimiter.len() == 1 && delimiter.is_ascii()) {
            if delimiter != "," {
                format.push(("delimiter".to_owned(), Yaml::str(delimiter)));
            }
        } else {
            notes.approximated(
                label,
                location,
                format!(
                    "the column delimiter {delimiter:?} is not a single character; `,` is used"
                ),
            );
        }
    }
    if let Some(quote) = value("quoteToken").or_else(|| value("quoteChar"))
        && quote != "\""
    {
        if quote.len() == 1 && quote.is_ascii() {
            format.push(("quote".into(), Yaml::str(quote)));
        } else {
            notes.approximated(
                label,
                location,
                format!("the quote {quote:?} is not a single character; `\"` is used"),
            );
        }
    }
    if let Some(record) = value("recordDelimiter")
        && !matches!(record, "\\n" | "\\r\\n" | "\\r")
    {
        notes.approximated(
            label,
            location,
            format!("the record delimiter {record:?} is not supported; records end at line breaks"),
        );
    }
    let skip = properties
        .and_then(|p| p.child("batchProperties"))
        .and_then(|b| b.number("batchSkipRecords"))
        .unwrap_or(0);
    if skip > 0 {
        format.push(("header".into(), Yaml::Bool(true)));
        if skip > 1 {
            notes.approximated(
                label,
                location,
                format!("{skip} header records are skipped in Mirth; OXIM reads one header row"),
            );
        }
    }
    (
        "delimited",
        (!format.is_empty()).then_some(Yaml::Map(format)),
    )
}

/// Notes on what a Mirth transformer does besides its steps.
fn transformer_notes(transformer: Option<&Element>, label: &str, notes: &mut Notes<'_>) {
    let Some(transformer) = transformer else {
        return;
    };
    if let (Some(inbound), Some(outbound)) = (
        transformer.value("inboundDataType"),
        transformer.value("outboundDataType"),
    ) && inbound != outbound
    {
        notes.approximated(
            label,
            &transformer.location,
            format!(
                "Mirth serializes the message as {outbound} after this transformer; OXIM keeps \
                 the {inbound} message, so use an encoder or a script step to change the format"
            ),
        );
    }
    if transformer.value("outboundTemplate").is_some() {
        notes.approximated(
            label,
            &transformer.location,
            "the outbound template is not used; steps that fill `tmp` from it need review",
        );
    }
}

fn has_enabled_elements(container: Option<&Element>) -> bool {
    container
        .and_then(|c| c.child("elements"))
        .is_some_and(|elements| {
            elements
                .children
                .iter()
                .any(|e| e.flag("enabled") != Some(false))
        })
}

/// The queue and retry policy of a destination.
fn queue(connector: &Element, label: &str, notes: &mut Notes<'_>) -> Yaml {
    let properties = connector.find(&["properties", "destinationConnectorProperties"]);
    let location = properties.map_or(connector.location.as_str(), |p| p.location.as_str());
    let flag = |name: &str| properties.and_then(|p| p.flag(name));
    let number = |name: &str| properties.and_then(|p| p.number(name));
    let queued = flag("queueEnabled").unwrap_or(false);
    let interval = number("retryIntervalMillis").unwrap_or(10_000);
    let retries = number("retryCount").unwrap_or(0);
    let delay = if interval == 0 {
        notes.approximated(
            label,
            location,
            "retrying without a pause became retrying every second",
        );
        duration(1000)
    } else {
        duration(interval)
    };
    let mut retry = vec![
        ("initial_delay".to_owned(), Yaml::str(delay.clone())),
        ("max_delay".into(), Yaml::str(delay.clone())),
        ("multiplier".into(), Yaml::Int(1)),
    ];
    if queued {
        notes.converted(
            label,
            location,
            format!("queued durably and retried every {delay} until delivered"),
        );
    } else {
        let attempts = retries.saturating_add(1);
        retry.push((
            "max_attempts".into(),
            Yaml::Int(i64::try_from(attempts).unwrap_or(i64::MAX)),
        ));
        notes.converted(
            label,
            location,
            format!(
                "OXIM queues every message durably; like Mirth without a queue, delivery gives up \
                 after {attempts} attempt(s), {delay} apart"
            ),
        );
    }
    if number("threadCount").is_some_and(|threads| threads > 1) {
        notes.approximated(
            label,
            location,
            "several queue threads became one: each OXIM destination queue is delivered in order",
        );
    }
    let mut entries = Vec::new();
    if flag("rotate") == Some(true) {
        entries.push(("ordering".to_owned(), Yaml::str("best_effort")));
    }
    entries.push(("retry".into(), Yaml::Map(retry)));
    Yaml::Map(entries)
}

/// How the source answers its sender.
fn response(
    source: Option<&Element>,
    connector: Option<&Connector>,
    destinations: &[(String, String)],
    notes: &mut Notes<'_>,
) -> Option<Yaml> {
    let connector = connector?;
    if !matches!(connector.kind, "mllp" | "tcp") {
        return None;
    }
    let properties = source?.find(&["properties", "sourceConnectorProperties"])?;
    let variable = properties.value("responseVariable").unwrap_or("None");
    let location = properties
        .child("responseVariable")
        .map_or(properties.location.as_str(), |e| e.location.as_str());
    let label = format!("source response ({variable})");
    let mllp = connector.kind == "mllp";
    if variable == "None" {
        if mllp {
            notes.approximated(
                &label,
                location,
                "Mirth sent no response; OXIM always acknowledges HL7 messages received over MLLP \
                 after storing them",
            );
        } else {
            notes.converted(&label, location, "no response is sent");
        }
        return None;
    }
    if variable.starts_with("Auto-generate") {
        if !mllp {
            notes.approximated(
                &label,
                location,
                "raw TCP sources send fixed bytes; set the source `response` setting",
            );
        } else if variable.contains("Destinations completed") {
            notes.approximated(
                &label,
                location,
                "OXIM acknowledges after storing the message durably, before the destinations \
                 are delivered",
            );
        } else {
            notes.converted(
                &label,
                location,
                "OXIM acknowledges each message after storing it durably",
            );
        }
        return None;
    }
    let target = variable
        .strip_prefix('d')
        .filter(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
        .and_then(|n| destinations.iter().find(|(meta, _)| meta == n))
        .map(|(_, id)| id.clone());
    match target {
        Some(id) if mllp => {
            notes.converted(
                &label,
                location,
                format!("the response of destination `{id}` is relayed to the sender"),
            );
            Some(Yaml::map([
                ("mode", Yaml::str("destination")),
                ("destination", Yaml::str(id)),
            ]))
        }
        _ => {
            notes.approximated(
                &label,
                location,
                "this response is not supported; OXIM acknowledges after storing the message",
            );
            None
        }
    }
}

fn script_notes(channel: &Element, notes: &mut Notes<'_>) {
    for (name, what, advice) in [
        (
            "preprocessingScript",
            "preprocessor script",
            "move the logic into a transformer `script` step",
        ),
        (
            "postprocessingScript",
            "postprocessor script",
            "move the logic into a destination or an alert",
        ),
        (
            "deployScript",
            "deploy script",
            "run the setup outside OXIM",
        ),
        (
            "undeployScript",
            "undeploy script",
            "run the cleanup outside OXIM",
        ),
    ] {
        if let Some(script) = channel.child(name)
            && !is_trivial(script.raw_text())
        {
            notes.unsupported(
                &format!("channel {what}"),
                &script.location,
                format!("channel {what}s are not supported; {advice}"),
            );
        }
    }
}

/// Channel properties; returns whether the channel starts with OXIM.
fn property_notes(channel: &Element, notes: &mut Notes<'_>) -> bool {
    let mut starts = true;
    let Some(properties) = channel.child("properties") else {
        return starts;
    };
    if let Some(state) = properties.value("initialState")
        && state != "STARTED"
    {
        starts = false;
        notes.approximated(
            &format!("initial state {state}"),
            &properties.location,
            "OXIM has no deployed-but-stopped state; the channel is written disabled",
        );
    }
    let flag = |name: &str| properties.flag(name) == Some(true);
    if flag("encryptData") {
        notes.approximated(
            "message encryption",
            &properties.location,
            "OXIM does not encrypt stored message content yet",
        );
    }
    if flag("removeContentOnCompletion") {
        notes.approximated(
            "remove content on completion",
            &properties.location,
            "OXIM keeps message content until `retention.contents_after` in oxim.yaml",
        );
    }
    if let Some(mode) = properties.value("messageStorageMode")
        && !matches!(mode, "DEVELOPMENT" | "PRODUCTION")
    {
        notes.approximated(
            &format!("message storage mode {mode}"),
            &properties.location,
            "OXIM stores the complete content of every message",
        );
    }
    if let Some(kind) = properties
        .find(&["attachmentProperties", "type"])
        .map(|e| e.raw_text().trim())
        .filter(|kind| !kind.is_empty() && *kind != "None")
    {
        notes.unsupported(
            &format!("attachment handler {kind}"),
            &properties.location,
            "attachment handlers are not supported; attachments stay in the message",
        );
    }
    if properties
        .child("metaDataColumns")
        .is_some_and(|columns| !columns.children.is_empty())
    {
        notes.approximated(
            "custom metadata columns",
            &properties.location,
            "custom metadata columns are not supported; their values are not stored",
        );
    }
    if let Some(pruning) = channel.find(&["exportData", "metadata", "pruningSettings"])
        && (pruning.value("pruneMetaDataDays").is_some()
            || pruning.value("pruneContentDays").is_some())
    {
        notes.approximated(
            "pruning settings",
            &pruning.location,
            "OXIM retention is set for the whole engine (`retention` in oxim.yaml)",
        );
    }
    starts
}

/// Whether the channel is enabled in Mirth.
pub(crate) fn enabled_in_mirth(channel: &Element) -> bool {
    channel
        .find(&["exportData", "metadata"])
        .and_then(|m| m.flag("enabled"))
        .or_else(|| channel.flag("enabled"))
        .unwrap_or(true)
}

/// The OXIM identifiers the channels of an export will get, by Mirth
/// channel id, so Channel Writers can name their target. Mirrors the
/// assignment in [`convert`].
pub(crate) fn planned_ids(
    channels: &[&Element],
    options: &ImportOptions,
) -> std::collections::BTreeMap<String, String> {
    let mut ids = Names::default();
    let mut planned = std::collections::BTreeMap::new();
    for channel in channels {
        if !enabled_in_mirth(channel) && !options.include_disabled {
            continue;
        }
        let name = channel.value("name").unwrap_or("unnamed channel");
        let id = ids.unique(&slug(name, "channel"));
        if let Some(mirth_id) = channel.value("id") {
            planned.insert(mirth_id.to_owned(), id);
        }
    }
    planned
}

/// Converts one channel. Returns `None` for a disabled channel that the
/// options skip.
pub(crate) fn convert(
    channel: &Element,
    globals: &Globals,
    ids: &mut Names,
    options: &ImportOptions,
    items: &mut Vec<ReportItem>,
) -> Result<Option<ImportedChannel>, MirthError> {
    let mirth_name = channel
        .value("name")
        .unwrap_or("unnamed channel")
        .to_owned();
    let mirth_id = channel.value("id").map(str::to_owned);
    let enabled_in_mirth = enabled_in_mirth(channel);
    if !enabled_in_mirth && !options.include_disabled {
        items.push(ReportItem {
            channel: None,
            element: format!("channel \"{mirth_name}\""),
            location: channel.location.clone(),
            outcome: Outcome::Converted,
            detail: "disabled in Mirth; skipped".into(),
        });
        return Ok(None);
    }
    let id = ids.unique(&slug(&mirth_name, "channel"));
    let templates = globals.templates_for(mirth_id.as_deref());
    let mut notes = Notes::new(Some(id.clone()), items, globals);

    let source = channel.child("sourceConnector");
    let source_transformer = source.and_then(|s| s.child("transformer"));
    let (data_type, format) = data_type(source_transformer, &mut notes);
    let hl7 = data_type == "hl7v2";
    let connector = match source {
        Some(source) => connectors::source(source, &mut notes),
        None => {
            notes.unsupported(
                "source connector",
                &channel.location,
                "the channel has no source connector",
            );
            None
        }
    };
    let scope = Scope {
        label: "source".into(),
        hl7,
        templates: &templates,
    };
    let filters = steps::filters(source.and_then(|s| s.child("filter")), &scope, &mut notes);
    let transformers = steps::transformers(source_transformer, &scope, &mut notes);
    transformer_notes(source_transformer, "source transformer", &mut notes);

    let mut destination_ids = Names::default();
    destination_ids.reserve("source");
    let mut destinations = Vec::new();
    let mut meta_ids: Vec<(String, String)> = Vec::new();
    let mut chained = Vec::new();
    let connectors_element = channel.child("destinationConnectors");
    for connector_element in connectors_element
        .map(|list| list.children_named("connector").collect::<Vec<_>>())
        .unwrap_or_default()
    {
        let name = connector_element.value("name").unwrap_or("destination");
        let label = format!("destination \"{name}\" ({})", transport(connector_element));
        if connector_element.flag("enabled") == Some(false) {
            notes.converted(
                &label,
                &connector_element.location,
                "disabled in Mirth; left out",
            );
            continue;
        }
        let Some(converted) = connectors::destination(connector_element, &label, &mut notes) else {
            continue;
        };
        let destination_id = destination_ids.unique(&slug(name, "destination"));
        let transformer = connector_element.child("transformer");
        let inbound_hl7 = transformer
            .and_then(|t| t.value("inboundDataType"))
            .is_none_or(|t| t == "HL7V2");
        let scope = Scope {
            label: format!("destination \"{name}\""),
            hl7: hl7 && inbound_hl7,
            templates: &templates,
        };
        let filters = steps::filters(connector_element.child("filter"), &scope, &mut notes);
        let transformers = steps::transformers(transformer, &scope, &mut notes);
        transformer_notes(transformer, &format!("{label} transformer"), &mut notes);
        let response_transformer = connector_element.child("responseTransformer");
        if has_enabled_elements(response_transformer) {
            notes.unsupported(
                &format!("{label} response transformer"),
                response_transformer.map_or("", |t| t.location.as_str()),
                "response transformers are not supported; the steps were left out",
            );
        }
        let queue = queue(connector_element, &label, &mut notes);
        if connector_element.flag("waitForPrevious") == Some(true) && !destinations.is_empty() {
            chained.push(name.to_owned());
        }
        if let Some(meta) = connector_element.value("metaDataId") {
            meta_ids.push((meta.to_owned(), destination_id.clone()));
        }
        let mut entries = vec![
            ("id".to_owned(), Yaml::str(destination_id)),
            ("type".into(), Yaml::str(converted.kind)),
        ];
        if !filters.is_empty() {
            entries.push(("filters".into(), Yaml::List(filters)));
        }
        if !transformers.is_empty() {
            entries.push(("transformers".into(), Yaml::List(transformers)));
        }
        entries.push(("queue".into(), queue));
        entries.push(("settings".into(), Yaml::Map(converted.settings)));
        destinations.push(Yaml::Map(entries));
    }
    if !chained.is_empty() {
        notes.approximated(
            "destination chain",
            connectors_element.map_or(channel.location.as_str(), |e| e.location.as_str()),
            format!(
                "Mirth waits for the previous destination before {}; OXIM delivers every \
                 destination from its own durable queue, so a destination cannot use an earlier \
                 destination's response and the order between destinations is not kept",
                chained
                    .iter()
                    .map(|name| format!("\"{name}\""))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
    }
    let response = response(source, connector.as_ref(), &meta_ids, &mut notes);
    script_notes(channel, &mut notes);
    let starts = property_notes(channel, &mut notes);

    let draft = connector.is_none();
    let enabled = enabled_in_mirth && starts && !draft;
    let file_name = if draft {
        format!("{id}.yaml.draft")
    } else {
        format!("{id}.yaml")
    };
    notes.add_first(
        Outcome::Converted,
        &format!("channel \"{mirth_name}\""),
        &channel.location,
        if enabled_in_mirth {
            format!("OXIM channel `{id}` in {file_name}")
        } else {
            format!("OXIM channel `{id}` in {file_name}, disabled as in Mirth")
        },
    );

    let mut source_entries = Vec::new();
    match &connector {
        Some(connector) => source_entries.push(("type".to_owned(), Yaml::str(connector.kind))),
        None => source_entries.push(("type".to_owned(), Yaml::str("unsupported"))),
    }
    source_entries.push(("data_type".into(), Yaml::str(data_type)));
    if let Some(format) = format {
        source_entries.push(("format".into(), format));
    }
    if let Some(response) = response {
        source_entries.push(("response".into(), response));
    }
    let settings = match connector {
        Some(connector) => connector.settings,
        None => vec![(
            "mirth_transport".to_owned(),
            Yaml::str(source.map_or_else(|| "none".to_owned(), transport)),
        )],
    };
    source_entries.push(("settings".into(), Yaml::Map(settings)));

    let mut entries = vec![
        ("id".to_owned(), Yaml::str(id.clone())),
        ("name".into(), Yaml::str(mirth_name.clone())),
    ];
    if let Some(description) = channel
        .script("description")
        .map(str::trim)
        .filter(|d| !d.is_empty())
    {
        entries.push((
            "description".into(),
            Yaml::str(crate::templates::normalize_newlines(description)),
        ));
    }
    entries.push(("enabled".into(), Yaml::Bool(enabled)));
    entries.push(("source".into(), Yaml::Map(source_entries)));
    if !filters.is_empty() {
        entries.push(("filters".into(), Yaml::List(filters)));
    }
    if !transformers.is_empty() {
        entries.push(("transformers".into(), Yaml::List(transformers)));
    }
    entries.push(("destinations".into(), Yaml::List(destinations)));

    let mut header = format!(
        "# Imported from Mirth Connect channel \"{}\"",
        mirth_name.replace(['\r', '\n'], " ")
    );
    if let Some(mirth_id) = &mirth_id {
        header.push_str(&format!(" ({mirth_id})"));
    }
    header.push_str(" by oxim-mirth.\n");
    header.push_str(&format!(
        "# Migration report: {} converted, {} approximated, {} unsupported item(s).\n",
        notes.outcomes(Outcome::Converted),
        notes.outcomes(Outcome::Approximated),
        notes.outcomes(Outcome::Unsupported),
    ));
    if draft {
        header.push_str(
            "# DRAFT: OXIM has no equivalent for the Mirth source connector yet. Replace the\n\
             # source and rename this file to .yaml; OXIM does not load .yaml.draft files.\n",
        );
    }
    let yaml = format!("{header}\n{}", yaml::to_string(&Yaml::Map(entries)));
    oxim_core::ChannelConfig::from_yaml(&yaml).map_err(|e| MirthError::Internal {
        channel: mirth_name.clone(),
        message: e.to_string(),
    })?;
    Ok(Some(ImportedChannel {
        id,
        mirth_id,
        mirth_name,
        file_name,
        yaml,
        enabled,
        draft,
    }))
}
