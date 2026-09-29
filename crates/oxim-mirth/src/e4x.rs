//! Recognizing the JavaScript expressions Mirth's rule builder, mapper and
//! message builder generate, so they can become declarative OXIM steps.

/// Largest index accepted in an expression.
const MAX_INDEX: usize = 999;

#[derive(Debug, PartialEq, Eq)]
enum Access {
    Name(String),
    Index(usize),
}

/// Splits `['A'][0]["B"]` into accessors; `None` when anything else
/// appears.
fn accessors(mut rest: &str) -> Option<Vec<Access>> {
    let mut out = Vec::new();
    while !rest.is_empty() {
        rest = rest.strip_prefix('[')?.trim_start();
        let end = rest.find(']')?;
        let inner = rest[..end].trim();
        rest = rest[end + 1..].trim_start();
        if let Some(text) = js_string(inner) {
            out.push(Access::Name(text));
        } else {
            let index: usize = inner.parse().ok()?;
            if index > MAX_INDEX {
                return None;
            }
            out.push(Access::Index(index));
        }
    }
    Some(out)
}

fn strip_conversion(expr: &str) -> &str {
    let expr = expr.trim().trim_end_matches(';').trim_end();
    for suffix in [".toString()", ".text()"] {
        if let Some(stripped) = expr.strip_suffix(suffix) {
            return stripped.trim_end();
        }
    }
    expr
}

fn is_segment(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.len() == 3
        && bytes[0].is_ascii_uppercase()
        && bytes[1..]
            .iter()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

/// The numbers after `SEG.` in `SEG.3.1`, checking the segment name.
fn numbers(name: &str, segment: &str, count: usize) -> Option<Vec<usize>> {
    let rest = name.strip_prefix(segment)?.strip_prefix('.')?;
    let parts: Vec<usize> = rest
        .split('.')
        .map(|part| part.parse().ok().filter(|n| (1..=MAX_INDEX).contains(n)))
        .collect::<Option<_>>()?;
    (parts.len() == count).then_some(parts)
}

/// The OXIM HL7 path of an E4X expression on the message, such as
/// `msg['PID']['PID.3']['PID.3.1'].toString()` (`PID-3.1`) or
/// `msg['OBX'][1]['OBX.5'][0]` (`OBX[2]-5[1]`). With `root`, the
/// expression must start with that variable (`msg` or `tmp`).
pub(crate) fn hl7_path(expr: &str, root: &str) -> Option<String> {
    let expr = strip_conversion(expr);
    let rest = expr.strip_prefix(root)?.trim_start();
    let accessors = accessors(rest)?;
    let mut iter = accessors.into_iter().peekable();
    let Some(Access::Name(segment)) = iter.next() else {
        return None;
    };
    if !is_segment(&segment) {
        return None;
    }
    let mut path = segment.clone();
    if let Some(Access::Index(occurrence)) = iter.peek() {
        if *occurrence > 0 {
            path.push_str(&format!("[{}]", occurrence + 1));
        }
        iter.next();
    }
    let Some(Access::Name(field)) = iter.next() else {
        return None;
    };
    let field = numbers(&field, &segment, 1)?;
    path.push_str(&format!("-{}", field[0]));
    if let Some(Access::Index(repetition)) = iter.peek() {
        path.push_str(&format!("[{}]", repetition + 1));
        iter.next();
    }
    if let Some(access) = iter.next() {
        let Access::Name(component) = access else {
            return None;
        };
        let parts = numbers(&component, &segment, 2)?;
        if parts[0] != field[0] {
            return None;
        }
        path.push_str(&format!(".{}", parts[1]));
        if let Some(access) = iter.next() {
            let Access::Name(sub) = access else {
                return None;
            };
            let sub = numbers(&sub, &segment, 3)?;
            if sub[0] != field[0] || sub[1] != parts[1] {
                return None;
            }
            path.push_str(&format!(".{}", sub[2]));
        }
    }
    iter.next().is_none().then_some(path)
}

/// The value of a JavaScript string literal (`'ADT'` or `"ADT"`).
pub(crate) fn js_string(expr: &str) -> Option<String> {
    let expr = expr.trim();
    let quote = expr.chars().next().filter(|c| *c == '\'' || *c == '"')?;
    let body = expr.strip_prefix(quote)?.strip_suffix(quote)?;
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                c @ ('\\' | '\'' | '"' | '/') => out.push(c),
                _ => return None,
            },
            c if c == quote => return None,
            c => out.push(c),
        }
    }
    Some(out)
}

/// A JavaScript string literal for `text`.
pub(crate) fn js_quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('\'');
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

/// Whether `name` can be an OXIM variable in templates (`{$name}`).
pub(crate) fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The variable an expression reads: `$('name')`, `$c('name')`,
/// `channelMap.get('name')` or `connectorMap.get('name')`.
pub(crate) fn js_variable(expr: &str) -> Option<String> {
    let expr = strip_conversion(expr);
    let inner = ["$(", "$c(", "$co(", "channelMap.get(", "connectorMap.get("]
        .iter()
        .find_map(|prefix| expr.strip_prefix(prefix))?
        .strip_suffix(')')?;
    js_string(inner).filter(|name| is_identifier(name))
}

/// Escapes text for an OXIM template (`{` and `}` doubled).
pub(crate) fn template_text(text: &str) -> String {
    text.replace('{', "{{").replace('}', "}}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_hl7_field_expressions() {
        for (expr, path) in [
            ("msg['PID']['PID.3']['PID.3.1'].toString()", "PID-3.1"),
            ("msg[\"MSH\"][\"MSH.9\"][\"MSH.9.1\"]", "MSH-9.1"),
            ("msg['OBX'][1]['OBX.5'].toString()", "OBX[2]-5"),
            ("msg['OBX'][0]['OBX.5'][0]", "OBX-5[1]"),
            ("msg['PID']['PID.3'][1]['PID.3.1']", "PID-3[2].1"),
            (
                "msg['PID']['PID.5']['PID.5.1']['PID.5.1.2'].text();",
                "PID-5.1.2",
            ),
            ("  msg [ 'PV1' ] [ 'PV1.2' ]  ", "PV1-2"),
        ] {
            assert_eq!(hl7_path(expr, "msg").as_deref(), Some(path), "{expr}");
        }
        for expr in [
            "msg['PID']",
            "msg['PID']['PV1.3']",
            "msg['PID']['PID.3']['PID.4.1']",
            "msg['pid']['pid.3']",
            "msg['PID']['PID.3'].toString().trim()",
            "tmp['PID']['PID.3']",
            "msg['PID']['PID.3'] + 'x'",
            "msg['PID']['PID.3'][10000]",
        ] {
            assert_eq!(hl7_path(expr, "msg"), None, "{expr}");
        }
        assert_eq!(
            hl7_path("tmp['PID']['PID.3']", "tmp").as_deref(),
            Some("PID-3")
        );
    }

    #[test]
    fn reads_literals_and_variables() {
        assert_eq!(js_string("'ADT'").as_deref(), Some("ADT"));
        assert_eq!(js_string(r#""a\"b\\c""#).as_deref(), Some("a\"b\\c"));
        assert_eq!(js_string("'a' + 'b'"), None);
        assert_eq!(js_string("ADT"), None);
        assert_eq!(js_quote("it's\n"), r"'it\'s\n'");
        assert_eq!(js_variable("$('patientId')").as_deref(), Some("patientId"));
        assert_eq!(
            js_variable("channelMap.get(\"mrn\").toString()").as_deref(),
            Some("mrn")
        );
        assert_eq!(js_variable("$('two words')"), None);
        assert_eq!(js_variable("$g('x')"), None);
        assert_eq!(template_text("{a}"), "{{a}}");
    }
}
