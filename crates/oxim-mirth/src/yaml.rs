//! A small YAML writer for channel files: keys keep their order, scripts
//! become literal blocks and every string is quoted when YAML could read
//! it as something else.

use std::fmt::Write as _;

/// A YAML value.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Yaml {
    Map(Vec<(String, Yaml)>),
    List(Vec<Yaml>),
    Str(String),
    Int(i64),
    Bool(bool),
}

impl Yaml {
    /// A map from key-value pairs.
    pub(crate) fn map<K: Into<String>>(entries: impl IntoIterator<Item = (K, Yaml)>) -> Self {
        Self::Map(
            entries
                .into_iter()
                .map(|(key, value)| (key.into(), value))
                .collect(),
        )
    }

    /// A string value.
    pub(crate) fn str(text: impl Into<String>) -> Self {
        Self::Str(text.into())
    }

    /// A list of strings.
    pub(crate) fn strings<S: Into<String>>(items: impl IntoIterator<Item = S>) -> Self {
        Self::List(
            items
                .into_iter()
                .map(|item| Self::Str(item.into()))
                .collect(),
        )
    }

    fn is_scalar(&self) -> bool {
        match self {
            Self::Str(text) => !text.contains('\n'),
            Self::Int(_) | Self::Bool(_) => true,
            Self::Map(_) | Self::List(_) => false,
        }
    }
}

/// Longest line a flow collection may take.
const FLOW_WIDTH: usize = 76;

/// Writes `value` as a YAML document.
pub(crate) fn to_string(value: &Yaml) -> String {
    let mut out = String::new();
    match value {
        Yaml::Map(entries) if !entries.is_empty() => write_entries(&mut out, entries, 0),
        other => {
            write_inline_or_block(&mut out, other, 0);
            out.push('\n');
        }
    }
    out
}

fn write_entries(out: &mut String, entries: &[(String, Yaml)], indent: usize) {
    for (index, (key, value)) in entries.iter().enumerate() {
        if index > 0 {
            push_indent(out, indent);
        }
        write_entry(out, key, value, indent);
    }
}

/// Writes `key: value` at the current position (the indentation is already
/// written) and ends the line.
fn write_entry(out: &mut String, key: &str, value: &Yaml, indent: usize) {
    out.push_str(&scalar(key));
    out.push(':');
    match value {
        Yaml::Map(entries) if !entries.is_empty() => match flow(value) {
            Some(text) if indent + key.len() + 2 + text.len() <= FLOW_WIDTH => {
                out.push(' ');
                out.push_str(&text);
                out.push('\n');
            }
            _ => {
                out.push('\n');
                push_indent(out, indent + 2);
                write_entries(out, entries, indent + 2);
            }
        },
        Yaml::List(items) if !items.is_empty() => match flow(value) {
            Some(text) if indent + key.len() + 2 + text.len() <= FLOW_WIDTH => {
                out.push(' ');
                out.push_str(&text);
                out.push('\n');
            }
            _ => {
                out.push('\n');
                write_items(out, items, indent);
            }
        },
        Yaml::Str(text) if text.contains('\n') => {
            out.push(' ');
            write_multiline(out, text, indent);
        }
        other => {
            out.push(' ');
            write_inline_or_block(out, other, indent);
            out.push('\n');
        }
    }
}

fn write_items(out: &mut String, items: &[Yaml], indent: usize) {
    for item in items {
        push_indent(out, indent);
        out.push_str("- ");
        match item {
            Yaml::Map(entries) if !entries.is_empty() => match flow(item) {
                Some(text) if indent + 2 + text.len() <= FLOW_WIDTH => {
                    out.push_str(&text);
                    out.push('\n');
                }
                _ => write_entries(out, entries, indent + 2),
            },
            Yaml::List(inner) if !inner.is_empty() => {
                out.push('\n');
                write_items(out, inner, indent + 2);
            }
            Yaml::Str(text) if text.contains('\n') => write_multiline(out, text, indent),
            other => {
                write_inline_or_block(out, other, indent + 2);
                out.push('\n');
            }
        }
    }
}

fn write_inline_or_block(out: &mut String, value: &Yaml, indent: usize) {
    match value {
        Yaml::Map(entries) if entries.is_empty() => out.push_str("{}"),
        Yaml::List(items) if items.is_empty() => out.push_str("[]"),
        Yaml::Str(text) => out.push_str(&scalar(text)),
        Yaml::Int(number) => {
            let _ = write!(out, "{number}");
        }
        Yaml::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
        Yaml::Map(_) | Yaml::List(_) => {
            // Not reached for non-empty collections, which callers write
            // as blocks; fall back to flow style.
            out.push_str(&flow(value).unwrap_or_default());
            let _ = indent;
        }
    }
}

/// A flow collection (`{a: 1, b: x}`) when every member is a single-line
/// scalar or a small flow collection itself.
fn flow(value: &Yaml) -> Option<String> {
    match value {
        Yaml::Map(entries) => {
            let parts = entries
                .iter()
                .map(|(key, value)| {
                    let inner = if value.is_scalar() {
                        scalar_value(value)
                    } else {
                        flow(value)?
                    };
                    Some(format!("{}: {inner}", scalar(key)))
                })
                .collect::<Option<Vec<_>>>()?;
            Some(format!("{{{}}}", parts.join(", ")))
        }
        Yaml::List(items) => {
            let parts = items
                .iter()
                .map(|item| {
                    if item.is_scalar() {
                        Some(scalar_value(item))
                    } else {
                        flow(item)
                    }
                })
                .collect::<Option<Vec<_>>>()?;
            Some(format!("[{}]", parts.join(", ")))
        }
        _ => None,
    }
}

fn scalar_value(value: &Yaml) -> String {
    match value {
        Yaml::Str(text) => scalar(text),
        Yaml::Int(number) => number.to_string(),
        Yaml::Bool(flag) => flag.to_string(),
        Yaml::Map(_) | Yaml::List(_) => String::new(),
    }
}

fn push_indent(out: &mut String, indent: usize) {
    out.extend(std::iter::repeat_n(' ', indent));
}

/// Words YAML 1.1 or 1.2 read as booleans or null.
const RESERVED: &[&str] = &[
    "true", "false", "yes", "no", "on", "off", "y", "n", "null", "~",
];

/// A single-line string, plain when that is unambiguous.
fn scalar(text: &str) -> String {
    if is_plain(text) {
        text.to_owned()
    } else {
        quoted(text)
    }
}

fn is_plain(text: &str) -> bool {
    let Some(first) = text.chars().next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_' || first == '/' || first == '$') {
        return false;
    }
    if text.ends_with(' ') {
        return false;
    }
    if RESERVED.iter().any(|word| text.eq_ignore_ascii_case(word)) {
        return false;
    }
    // `:` and `#` are left out entirely: they are only safe in some
    // positions and contexts, and quoting costs nothing.
    text.chars().all(|c| {
        c.is_ascii_alphanumeric()
            || matches!(
                c,
                ' ' | '_' | '-' | '.' | '/' | '@' | '$' | '(' | ')' | '+' | '=' | '^' | '~'
            )
    })
}

fn quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if needs_escape(c) => {
                let _ = write!(out, "\\u{:04X}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn needs_escape(c: char) -> bool {
    c.is_control() || matches!(c, '\u{feff}' | '\u{2028}' | '\u{2029}')
}

/// Writes a multi-line string as a literal block after `key: ` (or `- `)
/// and ends the line. Strings a literal block cannot hold exactly are
/// double-quoted instead.
fn write_multiline(out: &mut String, text: &str, indent: usize) {
    let blockable = !text
        .chars()
        .any(|c| c != '\n' && c != '\t' && needs_escape(c))
        && !text.contains('\r')
        && text.lines().any(|line| !line.trim().is_empty());
    if !blockable {
        out.push_str(&quoted(text));
        out.push('\n');
        return;
    }
    let trailing = text.len() - text.trim_end_matches('\n').len();
    let chomp = match trailing {
        0 => "-",
        1 => "",
        _ => "+",
    };
    let first = text
        .lines()
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let indicator = if first.starts_with(' ') { "2" } else { "" };
    out.push('|');
    out.push_str(indicator);
    out.push_str(chomp);
    out.push('\n');
    let body = text.trim_end_matches('\n');
    for line in body.split('\n') {
        if !line.is_empty() {
            push_indent(out, indent + 2);
            out.push_str(line);
        }
        out.push('\n');
    }
    for _ in 1..trailing {
        out.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> serde_json::Value {
        serde_saphyr::from_str(text).unwrap()
    }

    #[test]
    fn round_trips_through_a_yaml_parser() {
        let script = "// Mirth step\n\tif (x) {\n  y = 'a: b';\n}\n\nreturn true;";
        let value = Yaml::map([
            ("id", Yaml::str("adt-in")),
            ("name", Yaml::str("ADT: inbound #1")),
            ("enabled", Yaml::Bool(false)),
            ("listen", Yaml::str("0.0.0.0:6661")),
            ("yes", Yaml::str("no")),
            ("count", Yaml::Int(3)),
            ("empty", Yaml::str("")),
            ("list", Yaml::strings(["ADT", "O,R,U", "true"])),
            (
                "steps",
                Yaml::List(vec![
                    Yaml::map([("type", Yaml::str("script")), ("source", Yaml::str(script))]),
                    Yaml::map([
                        ("path", Yaml::str("PID-3.1")),
                        ("equals", Yaml::str("x\"y")),
                    ]),
                    Yaml::map([("source", Yaml::str("  indented first\nline\n\n"))]),
                    Yaml::map([("source", Yaml::str("a\r\nb"))]),
                ]),
            ),
            (
                "nested",
                Yaml::map([("inner", Yaml::map([("deep", Yaml::str("{x}"))]))]),
            ),
            ("nothing", Yaml::map(Vec::<(String, Yaml)>::new())),
        ]);
        let text = to_string(&value);
        let parsed = parse(&text);
        assert_eq!(parsed["id"], "adt-in", "{text}");
        assert_eq!(parsed["name"], "ADT: inbound #1");
        assert_eq!(parsed["enabled"], false);
        assert_eq!(parsed["listen"], "0.0.0.0:6661");
        assert_eq!(parsed["yes"], "no");
        assert_eq!(parsed["count"], 3);
        assert_eq!(parsed["empty"], "");
        assert_eq!(parsed["list"], serde_json::json!(["ADT", "O,R,U", "true"]));
        assert_eq!(parsed["steps"][0]["source"], script, "{text}");
        assert_eq!(parsed["steps"][1]["equals"], "x\"y");
        assert_eq!(
            parsed["steps"][2]["source"], "  indented first\nline\n\n",
            "{text}"
        );
        assert_eq!(parsed["steps"][3]["source"], "a\r\nb");
        assert_eq!(parsed["nested"]["inner"]["deep"], "{x}");
        assert_eq!(parsed["nothing"], serde_json::json!({}));
        assert!(text.contains("source: |-\n"), "{text}");
    }
}
