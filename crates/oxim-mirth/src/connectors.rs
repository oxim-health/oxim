//! Mirth source and destination connectors as OXIM connectors.

use crate::context::Notes;
use crate::xml::Element;
use crate::yaml::Yaml;

/// An OXIM connector: its type and settings.
#[derive(Debug, Clone)]
pub(crate) struct Connector {
    pub(crate) kind: &'static str,
    pub(crate) settings: Vec<(String, Yaml)>,
}

/// The default body Mirth sends: the encoded message itself.
const ENCODED_DATA: &str = "${message.encodedData}";

/// The Mirth connector class, without its package.
pub(crate) fn class_name(connector: &Element) -> &str {
    connector
        .child("properties")
        .and_then(|properties| properties.attribute("class"))
        .map_or("", |class| class.rsplit('.').next().unwrap_or(class))
}

/// The connector's transport name, as Mirth shows it.
pub(crate) fn transport(connector: &Element) -> String {
    connector
        .value("transportName")
        .map(str::to_owned)
        .unwrap_or_else(|| {
            let class = class_name(connector);
            class.strip_suffix("Properties").unwrap_or(class).to_owned()
        })
}

/// Milliseconds as OXIM duration text.
pub(crate) fn duration(millis: u64) -> String {
    if millis.is_multiple_of(1000) {
        format!("{}s", millis / 1000)
    } else {
        format!("{millis}ms")
    }
}

/// Mirth's hex byte notation (`0B`, `1C0D`) as OXIM byte text (`hex:0B`).
fn hex_bytes(text: &str) -> Option<String> {
    let text: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let text = text
        .strip_prefix("0x")
        .or_else(|| text.strip_prefix("0X"))
        .unwrap_or(&text);
    (!text.is_empty()
        && text.len().is_multiple_of(2)
        && text.chars().all(|c| c.is_ascii_hexdigit()))
    .then(|| format!("hex:{}", text.to_ascii_uppercase()))
}

fn address(host: &str, port: &str) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// How a TCP connector frames messages.
enum Framing {
    /// Standard MLLP.
    Mllp,
    /// Start and end bytes (or none) on a raw stream.
    Raw {
        start: Option<String>,
        end: Option<String>,
    },
}

fn framing(properties: &Element, notes: &mut Notes<'_>, label: &str) -> Option<Framing> {
    let Some(mode) = properties.child("transmissionModeProperties") else {
        return Some(Framing::Mllp);
    };
    let plugin = mode
        .value("pluginPointName")
        .map(str::to_owned)
        .unwrap_or_else(|| mode.attribute("class").unwrap_or_default().to_owned());
    let start = mode.value("startOfMessageBytes").map(str::to_owned);
    let end = mode.value("endOfMessageBytes").map(str::to_owned);
    let upper = |value: &Option<String>| value.as_deref().map(str::to_ascii_uppercase);
    if plugin == "MLLP" || plugin.to_ascii_lowercase().contains("mllpmode") {
        let standard = matches!(upper(&start).as_deref(), None | Some("0B"))
            && matches!(upper(&end).as_deref(), None | Some("1C0D"));
        if standard {
            if mode.flag("useMLLPv2") == Some(true) {
                notes.converted(
                    label,
                    &mode.location,
                    "MLLP release 2 commit acknowledgments are handled by the OXIM MLLP connectors",
                );
            }
            return Some(Framing::Mllp);
        }
        notes.approximated(
            label,
            &mode.location,
            "MLLP with non-standard frame bytes became raw TCP framing with the same bytes; \
             OXIM's raw TCP connectors do not create or check HL7 acknowledgments",
        );
    } else if plugin != "Basic" && !plugin.to_ascii_lowercase().contains("framemode") {
        notes.unsupported(
            label,
            &mode.location,
            format!("the transmission mode {plugin:?} is not supported"),
        );
        return None;
    }
    Some(Framing::Raw {
        start: start.as_deref().and_then(hex_bytes),
        end: end.as_deref().and_then(hex_bytes),
    })
}

fn raw_framing(start: Option<String>, end: Option<String>) -> Yaml {
    match (start, end) {
        (start, Some(end)) => {
            let mut entries = vec![("mode".to_owned(), Yaml::str("delimited"))];
            if let Some(start) = start {
                entries.push(("start".into(), Yaml::str(start)));
            }
            entries.push(("end".into(), Yaml::str(end)));
            Yaml::Map(entries)
        }
        _ => Yaml::map([("mode", Yaml::str("none"))]),
    }
}

/// Reads a setting, replacing `${name}` configuration placeholders and
/// reporting the ones without a value.
fn setting(notes: &mut Notes<'_>, label: &str, element: &Element, name: &str) -> Option<String> {
    let raw = element.value(name)?;
    let child = element.child(name).map_or("", |c| c.location.as_str());
    Some(notes.resolve(raw, label, child))
}

fn charset_note(properties: &Element, notes: &mut Notes<'_>, label: &str) {
    if let Some(charset) = properties.value("charsetEncoding")
        && !matches!(charset, "DEFAULT_ENCODING" | "UTF-8" | "utf-8")
        && let Some(child) = properties.child("charsetEncoding")
    {
        notes.approximated(
            label,
            &child.location,
            format!(
                "the character set {charset} is not configured per connector in OXIM; HL7 v2 \
                 messages use MSH-18 and other data UTF-8 unless the channel format sets an encoding"
            ),
        );
    }
}

fn template_note(properties: &Element, element: &str, notes: &mut Notes<'_>, label: &str) {
    if let Some(template) = properties.value(element)
        && template != ENCODED_DATA
        && let Some(child) = properties.child(element)
    {
        notes.approximated(
            label,
            &child.location,
            format!(
                "the template {template:?} is not applied; OXIM sends the encoded message \
                 (use a transformer or encoder to build other content)"
            ),
        );
    }
}

/// Converts a source connector; `None` when OXIM has no equivalent (the
/// reason is reported).
pub(crate) fn source(connector: &Element, notes: &mut Notes<'_>) -> Option<Connector> {
    let label = format!("source connector ({})", transport(connector));
    let properties = connector.child("properties")?;
    match class_name(connector) {
        "TcpReceiverProperties" => tcp_receiver(properties, notes, &label),
        "FileReceiverProperties" => file_receiver(properties, notes, &label),
        "HttpReceiverProperties" => http_receiver(properties, notes, &label),
        "VmReceiverProperties" => {
            notes.converted(
                &label,
                &properties.location,
                "OXIM channel source: receives what other channels' channel destinations send",
            );
            Some(Connector {
                kind: "channel",
                settings: Vec::new(),
            })
        }
        class => {
            notes.unsupported(
                &label,
                &connector.location,
                format!(
                    "OXIM has no source connector for {} ({class}) yet; the channel was written \
                     as a draft",
                    transport(connector)
                ),
            );
            None
        }
    }
}

fn tcp_receiver(properties: &Element, notes: &mut Notes<'_>, label: &str) -> Option<Connector> {
    if properties.flag("serverMode") == Some(false) {
        notes.unsupported(
            label,
            &properties.location,
            "a TCP listener in client mode (connecting out to the sender) is not supported; \
             OXIM listens for connections",
        );
        return None;
    }
    let listener = properties.child("listenerConnectorProperties");
    let host = listener
        .and_then(|l| setting(notes, label, l, "host"))
        .unwrap_or_else(|| "0.0.0.0".to_owned());
    let Some(port) = listener.and_then(|l| setting(notes, label, l, "port")) else {
        notes.unsupported(label, &properties.location, "the listener has no port");
        return None;
    };
    let listen = address(&host, &port);
    let framing = framing(properties, notes, label)?;
    charset_note(properties, notes, label);
    if let Some(value) = properties.value("respondOnNewConnection")
        && value != "0"
        && let Some(child) = properties.child("respondOnNewConnection")
    {
        notes.approximated(
            label,
            &child.location,
            "responses on a new connection are not supported; OXIM answers on the connection \
             the message arrived on",
        );
    }
    if let Some(source) = properties.child("sourceConnectorProperties")
        && source.flag("processBatch") == Some(true)
    {
        notes.unsupported(
            label,
            &source.location,
            "batch processing is not supported; every received unit is one message",
        );
    }
    let mut settings = vec![("listen".to_owned(), Yaml::str(listen.clone()))];
    let kind = match framing {
        Framing::Mllp => "mllp",
        Framing::Raw { start, end } => {
            settings.push(("framing".into(), raw_framing(start, end)));
            "tcp"
        }
    };
    if let Some(max) = properties.number("maxConnections").filter(|n| *n > 0) {
        settings.push((
            "max_connections".into(),
            Yaml::Int(i64::try_from(max).unwrap_or(i64::MAX)),
        ));
    }
    notes.converted(
        label,
        &properties.location,
        format!("OXIM {kind} source listening on {listen}"),
    );
    Some(Connector { kind, settings })
}

fn file_receiver(properties: &Element, notes: &mut Notes<'_>, label: &str) -> Option<Connector> {
    let scheme = properties.value("scheme").unwrap_or("FILE");
    if !scheme.eq_ignore_ascii_case("file") {
        notes.unsupported(
            label,
            &properties.location,
            format!(
                "reading files over {scheme} is not supported yet (planned: FTP, SFTP, SMB, S3); \
                 the channel was written as a draft"
            ),
        );
        return None;
    }
    let Some(directory) = setting(notes, label, properties, "host") else {
        notes.unsupported(
            label,
            &properties.location,
            "the file reader has no directory",
        );
        return None;
    };
    let mut settings = vec![("directory".to_owned(), Yaml::str(directory.clone()))];
    if let Some(filter) = setting(notes, label, properties, "fileFilter") {
        if properties.flag("regex") == Some(true) {
            notes.approximated(
                label,
                &properties.location,
                format!(
                    "the regular expression file filter {filter:?} became `*`; OXIM patterns use \
                     `*` and `?` wildcards"
                ),
            );
        } else if filter != "*" {
            settings.push(("pattern".into(), Yaml::str(filter)));
        }
    }
    let poll = properties.child("pollConnectorProperties");
    let interval = poll
        .and_then(|p| p.number("pollingFrequency"))
        .or_else(|| properties.number("pollingFrequency"));
    match poll
        .and_then(|p| p.value("pollingType"))
        .unwrap_or("INTERVAL")
    {
        "INTERVAL" => {
            if let Some(millis) = interval.filter(|ms| *ms > 0) {
                settings.push(("poll_interval".into(), Yaml::str(duration(millis))));
            }
        }
        other => notes.approximated(
            label,
            &properties.location,
            format!("polling by {other} schedule became polling every 5 seconds"),
        ),
    }
    match properties.value("sortBy").unwrap_or("date") {
        "name" => settings.push(("sort".into(), Yaml::str("name"))),
        "size" => {
            settings.push(("sort".into(), Yaml::str("name")));
            notes.approximated(
                label,
                &properties.location,
                "sorting by size became sorting by name",
            );
        }
        _ => settings.push(("sort".into(), Yaml::str("modified"))),
    }
    let processed = |notes: &mut Notes<'_>| -> String {
        setting(notes, label, properties, "moveToDirectory")
            .unwrap_or_else(|| format!("{}/processed", directory.trim_end_matches(['/', '\\'])))
    };
    match properties.value("afterProcessingAction").unwrap_or("NONE") {
        "DELETE" => settings.push(("after".into(), Yaml::str("delete"))),
        "MOVE" => {
            let target = processed(notes);
            settings.push(("after".into(), Yaml::str("move")));
            settings.push(("processed_directory".into(), Yaml::str(target)));
            if properties.value("moveToFileName").is_some() {
                notes.approximated(
                    label,
                    &properties.location,
                    "moved files keep their names; renaming on move is not supported",
                );
            }
        }
        _ => {
            let target = processed(notes);
            notes.approximated(
                label,
                &properties.location,
                format!(
                    "Mirth leaves read files in place; OXIM must move or delete them, so they are \
                     moved to {target}"
                ),
            );
            settings.push(("after".into(), Yaml::str("move")));
            settings.push(("processed_directory".into(), Yaml::str(target)));
        }
    }
    if let Some(error) = setting(notes, label, properties, "errorMoveToDirectory") {
        settings.push(("error_directory".into(), Yaml::str(error)));
    }
    if properties.flag("ignoreFileSizeMaximum") == Some(false)
        && let Some(max) = properties.number("fileSizeMaximum").filter(|n| *n > 0)
    {
        settings.push((
            "max_file_size".into(),
            Yaml::Int(i64::try_from(max).unwrap_or(i64::MAX)),
        ));
    }
    if properties.flag("directoryRecursion") == Some(true) {
        notes.approximated(
            label,
            &properties.location,
            "subdirectories are not read; OXIM reads the directory itself",
        );
    }
    if properties.flag("ignoreDot") == Some(false) {
        notes.approximated(
            label,
            &properties.location,
            "hidden files (names starting with a dot) are always ignored",
        );
    }
    charset_note(properties, notes, label);
    notes.converted(
        label,
        &properties.location,
        format!("OXIM file source reading {directory}"),
    );
    Some(Connector {
        kind: "file",
        settings,
    })
}

/// Converts a destination connector; `None` when OXIM has no equivalent
/// (the reason is reported).
pub(crate) fn destination(
    connector: &Element,
    label: &str,
    notes: &mut Notes<'_>,
) -> Option<Connector> {
    let properties = connector.child("properties")?;
    match class_name(connector) {
        "TcpDispatcherProperties" => tcp_dispatcher(properties, notes, label),
        "FileDispatcherProperties" => file_dispatcher(properties, notes, label),
        "HttpDispatcherProperties" => http_dispatcher(properties, notes, label),
        "VmDispatcherProperties" => channel_writer(properties, notes, label),
        class => {
            notes.unsupported(
                label,
                &connector.location,
                format!(
                    "OXIM has no destination connector for {} ({class}) yet; the destination \
                     was left out",
                    transport(connector)
                ),
            );
            None
        }
    }
}

fn http_receiver(properties: &Element, notes: &mut Notes<'_>, label: &str) -> Option<Connector> {
    let listener = properties.child("listenerConnectorProperties");
    let host = listener
        .and_then(|l| setting(notes, label, l, "host"))
        .unwrap_or_else(|| "0.0.0.0".to_owned());
    let Some(port) = listener.and_then(|l| setting(notes, label, l, "port")) else {
        notes.unsupported(label, &properties.location, "the listener has no port");
        return None;
    };
    let listen = address(&host, &port);
    let mut settings = vec![("listen".to_owned(), Yaml::str(listen.clone()))];
    if let Some(path) = setting(notes, label, properties, "contextPath")
        .map(|path| path.trim().to_owned())
        .filter(|path| !path.is_empty() && path != "/")
    {
        let path = if path.starts_with('/') {
            path
        } else {
            format!("/{path}")
        };
        settings.push(("path".into(), Yaml::str(path)));
    }
    if let Some(status) = properties.value("responseStatusCode").map(str::trim)
        && !status.is_empty()
    {
        match status.parse::<u16>() {
            Ok(code) if (200..300).contains(&code) => {
                settings.push(("status".into(), Yaml::Int(i64::from(code))));
            }
            _ => notes.approximated(
                label,
                &properties.location,
                format!(
                    "the response status {status:?} is not a fixed 2xx code; OXIM answers 200, \
                     or 503 when the message could not be stored"
                ),
            ),
        }
    }
    notes.approximated(
        label,
        &properties.location,
        "OXIM accepts POST and PUT requests (set `methods` for others) and stores the raw body",
    );
    if properties.flag("xmlBody") == Some(true) {
        notes.approximated(
            label,
            &properties.location,
            "Mirth converted requests to XML with headers and parameters; OXIM stores the body \
             and records the method, path and content type as metadata",
        );
    }
    if properties.flag("parseMultipart") == Some(true) {
        notes.approximated(
            label,
            &properties.location,
            "multipart bodies are stored as received",
        );
    }
    if let Some(auth) = properties.find(&["pluginProperties"])
        && auth
            .children
            .iter()
            .any(|plugin| plugin.name.contains("httpauth") && !plugin.name.contains("NoneHttpAuth"))
    {
        notes.approximated(
            label,
            &auth.location,
            "authentication settings were not copied; set `auth` (basic or bearer) on the source",
        );
    }
    template_note(properties, "responseContentType", notes, label);
    charset_note(properties, notes, label);
    notes.converted(
        label,
        &properties.location,
        format!("OXIM http source listening on {listen}"),
    );
    Some(Connector {
        kind: "http",
        settings,
    })
}

fn channel_writer(properties: &Element, notes: &mut Notes<'_>, label: &str) -> Option<Connector> {
    let target = properties.value("channelId").map(str::trim).unwrap_or("");
    if target.is_empty() || target.eq_ignore_ascii_case("none") {
        notes.unsupported(
            label,
            &properties.location,
            "the channel writer has no target channel; the destination was left out",
        );
        return None;
    }
    let channel = match notes.channel_id(target) {
        Some(id) => {
            notes.converted(
                label,
                &properties.location,
                format!("OXIM channel destination to the imported channel {id}"),
            );
            id
        }
        None => {
            let placeholder = format!(
                "mirth-{}",
                target
                    .chars()
                    .filter(char::is_ascii_alphanumeric)
                    .take(8)
                    .collect::<String>()
                    .to_ascii_lowercase()
            );
            notes.approximated(
                label,
                &properties.location,
                format!(
                    "the target channel {target} is not part of this export; the destination \
                     names {placeholder}, replace it with the target's OXIM channel id"
                ),
            );
            placeholder
        }
    };
    template_note(properties, "channelTemplate", notes, label);
    Some(Connector {
        kind: "channel",
        settings: vec![("channel".to_owned(), Yaml::str(channel))],
    })
}

fn millis_setting(properties: &Element, name: &str) -> Option<String> {
    properties.number(name).filter(|ms| *ms > 0).map(duration)
}

fn tcp_dispatcher(properties: &Element, notes: &mut Notes<'_>, label: &str) -> Option<Connector> {
    if properties.flag("serverMode") == Some(true) {
        notes.unsupported(
            label,
            &properties.location,
            "a TCP sender in server mode (waiting for the receiver to connect) is not supported; \
             the destination was left out",
        );
        return None;
    }
    let host = setting(notes, label, properties, "remoteAddress");
    let port = setting(notes, label, properties, "remotePort");
    let (Some(host), Some(port)) = (host, port) else {
        notes.unsupported(
            label,
            &properties.location,
            "the sender has no remote address or port; the destination was left out",
        );
        return None;
    };
    let target = address(&host, &port);
    let framing = framing(properties, notes, label)?;
    let ignore_response = properties.flag("ignoreResponse") == Some(true);
    let timeout = millis_setting(properties, "responseTimeout");
    let mut settings = vec![("target".to_owned(), Yaml::str(target.clone()))];
    let kind = match framing {
        Framing::Mllp => {
            if ignore_response {
                settings.push(("ack".into(), Yaml::str("none")));
            } else if let Some(timeout) = timeout {
                settings.push(("ack_timeout".into(), Yaml::str(timeout)));
            }
            "mllp"
        }
        Framing::Raw { start, end } => {
            settings.push(("framing".into(), raw_framing(start, end)));
            if !ignore_response {
                settings.push(("wait_for_response".into(), Yaml::Bool(true)));
                if let Some(timeout) = timeout {
                    settings.push(("response_timeout".into(), Yaml::str(timeout)));
                }
            }
            "tcp"
        }
    };
    template_note(properties, "template", notes, label);
    charset_note(properties, notes, label);
    notes.converted(
        label,
        &properties.location,
        format!("OXIM {kind} destination sending to {target}"),
    );
    Some(Connector { kind, settings })
}

/// Translates a Mirth output file name pattern into an OXIM file name
/// template. Returns the template and the variables that had no
/// equivalent.
fn file_name(pattern: &str) -> (String, Vec<String>, bool) {
    let mut out = String::new();
    let mut missing = Vec::new();
    let mut unique = false;
    let mut rest = pattern;
    while let Some(start) = rest.find("${") {
        out.push_str(&crate::e4x::template_text(&rest[..start]));
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            out.push_str(&crate::e4x::template_text(&rest[start..]));
            rest = "";
            break;
        };
        let name = &after[..end];
        rest = &after[end + 1..];
        let replacement = match name {
            "message.messageId" | "messageId" | "UUID" | "COUNT" | "SYSTIME" => {
                unique = true;
                "{message_id}"
            }
            "message.channelId" | "channelId" | "channelName" | "message.channelName" => {
                "{channel}"
            }
            "DATE" => "{timestamp}",
            _ if name.starts_with("date.get(") || name.starts_with("DATE") => "{timestamp}",
            _ => {
                missing.push(name.to_owned());
                unique = true;
                "{message_id}"
            }
        };
        out.push_str(replacement);
    }
    out.push_str(&crate::e4x::template_text(rest));
    (out, missing, unique)
}

fn file_dispatcher(properties: &Element, notes: &mut Notes<'_>, label: &str) -> Option<Connector> {
    let scheme = properties.value("scheme").unwrap_or("FILE");
    if !scheme.eq_ignore_ascii_case("file") {
        notes.unsupported(
            label,
            &properties.location,
            format!(
                "writing files over {scheme} is not supported yet (planned: FTP, SFTP, SMB, S3); \
                 the destination was left out"
            ),
        );
        return None;
    }
    let Some(directory) = setting(notes, label, properties, "host") else {
        notes.unsupported(
            label,
            &properties.location,
            "the file writer has no directory; the destination was left out",
        );
        return None;
    };
    let mut settings = vec![("directory".to_owned(), Yaml::str(directory.clone()))];
    let append = properties.flag("outputAppend").unwrap_or(true);
    if let Some(pattern) = properties.value("outputPattern") {
        let (mut template, missing, unique) = file_name(pattern);
        if !missing.is_empty() {
            notes.approximated(
                label,
                &properties.location,
                format!(
                    "the file name variables {} have no OXIM equivalent and became the message \
                     identifier",
                    missing.join(", ")
                ),
            );
        }
        if append && !unique {
            template = match template.rsplit_once('.') {
                Some((stem, extension)) => format!("{stem}-{{message_id}}.{extension}"),
                None => format!("{template}-{{message_id}}"),
            };
            notes.approximated(
                label,
                &properties.location,
                "appending messages to one file is not supported; each message is written to \
                 its own file named with the message identifier",
            );
        }
        settings.push(("filename".into(), Yaml::str(template)));
    }
    if !append && properties.flag("errorOnExists") != Some(true) {
        settings.push(("overwrite".into(), Yaml::Bool(true)));
    }
    template_note(properties, "template", notes, label);
    charset_note(properties, notes, label);
    notes.converted(
        label,
        &properties.location,
        format!("OXIM file destination writing to {directory}"),
    );
    Some(Connector {
        kind: "file",
        settings,
    })
}

/// `name -> values` pairs of a Mirth map (`<entry><string>k</string>...`).
fn entries(map: &Element) -> Vec<(String, Vec<String>)> {
    map.children_named("entry")
        .filter_map(|entry| {
            let mut strings = entry.children_named("string");
            let key = strings.next()?.raw_text().trim().to_owned();
            let mut values: Vec<String> = strings.map(|s| s.raw_text().to_owned()).collect();
            if let Some(list) = entry.child("list") {
                values.extend(list.strings());
            }
            Some((key, values))
        })
        .collect()
}

/// Percent-encodes a URL query component.
fn query_escape(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn http_dispatcher(properties: &Element, notes: &mut Notes<'_>, label: &str) -> Option<Connector> {
    let Some(mut url) = setting(notes, label, properties, "host") else {
        notes.unsupported(
            label,
            &properties.location,
            "the HTTP sender has no URL; the destination was left out",
        );
        return None;
    };
    let method = properties
        .value("method")
        .unwrap_or("post")
        .to_ascii_uppercase();
    if method != "POST" && method != "PUT" {
        notes.unsupported(
            label,
            &properties.location,
            format!("the HTTP method {method} is not supported (POST and PUT are); the destination was left out"),
        );
        return None;
    }
    if let Some(parameters) = properties.child("parameters") {
        let pairs: Vec<String> = entries(parameters)
            .into_iter()
            .flat_map(|(key, values)| {
                values
                    .into_iter()
                    .map(move |value| format!("{}={}", query_escape(&key), query_escape(&value)))
            })
            .collect();
        if !pairs.is_empty() {
            url.push(if url.contains('?') { '&' } else { '?' });
            url.push_str(&pairs.join("&"));
        }
    }
    let mut settings = vec![("url".to_owned(), Yaml::str(url.clone()))];
    if method == "PUT" {
        settings.push(("method".into(), Yaml::str("PUT")));
    }
    if let Some(headers) = properties.child("headers") {
        let headers: Vec<(String, Yaml)> = entries(headers)
            .into_iter()
            .map(|(key, values)| (key, Yaml::str(values.join(", "))))
            .collect();
        if !headers.is_empty() {
            settings.push(("headers".into(), Yaml::Map(headers)));
        }
    }
    if let Some(content_type) = properties.value("contentType") {
        settings.push(("content_type".into(), Yaml::str(content_type)));
    }
    if let Some(timeout) = millis_setting(properties, "socketTimeout") {
        settings.push(("timeout".into(), Yaml::str(timeout)));
    }
    if properties.flag("useAuthentication") == Some(true) {
        notes.approximated(
            label,
            &properties.location,
            "credentials were not copied; add an `Authorization` header to the destination",
        );
    }
    if properties.flag("useProxyServer") == Some(true) {
        notes.approximated(
            label,
            &properties.location,
            "the proxy server is not used; OXIM connects directly",
        );
    }
    if properties.flag("multipart") == Some(true) {
        notes.approximated(
            label,
            &properties.location,
            "multipart requests are not supported; the message is the request body",
        );
    }
    template_note(properties, "content", notes, label);
    notes.converted(
        label,
        &properties.location,
        format!("OXIM http destination sending {method} requests to {url}"),
    );
    Some(Connector {
        kind: "http",
        settings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_bytes_and_file_names() {
        assert_eq!(hex_bytes("0B").as_deref(), Some("hex:0B"));
        assert_eq!(hex_bytes("1c0d").as_deref(), Some("hex:1C0D"));
        assert_eq!(hex_bytes("0x1C"), Some("hex:1C".into()));
        assert_eq!(hex_bytes("ABC"), None);
        assert_eq!(hex_bytes(""), None);
        assert_eq!(duration(10_000), "10s");
        assert_eq!(duration(1500), "1500ms");
        assert_eq!(address("::1", "80"), "[::1]:80");
        assert_eq!(
            file_name("${message.messageId}.hl7"),
            ("{message_id}.hl7".into(), vec![], true)
        );
        assert_eq!(
            file_name("out_${date.get('yyyyMMdd')}_${patientId}.txt"),
            (
                "out_{timestamp}_{message_id}.txt".into(),
                vec!["patientId".into()],
                true
            )
        );
        assert_eq!(
            file_name("all{x}.txt"),
            ("all{{x}}.txt".into(), vec![], false)
        );
        assert_eq!(query_escape("a b&c"), "a%20b%26c");
    }
}
