//! Mirth filter rules and transformer steps as OXIM filters and
//! transformers.
//!
//! Rules and steps the rule builder, mapper and message builder generate
//! for HL7 v2 fields become declarative steps (`path-equals`, `path-in`,
//! `path-exists`, `condition`, `map`). Everything else becomes a `script`
//! step in Mirth compatibility mode, with the code templates it calls.

use std::sync::LazyLock;

use regex::Regex;

use crate::context::Notes;
use crate::e4x::{hl7_path, is_identifier, js_quote, js_string, js_variable, template_text};
use crate::templates::{CodeTemplate, normalize_newlines, with_templates};
use crate::xml::Element;
use crate::yaml::Yaml;

/// Where the steps run.
pub(crate) struct Scope<'a> {
    /// `source` or `destination "Name"`, for the report.
    pub(crate) label: String,
    /// Whether the data type is HL7 v2, so E4X field access maps to paths.
    pub(crate) hl7: bool,
    /// The code templates available to the channel.
    pub(crate) templates: &'a [&'a CodeTemplate],
}

/// Mirth and Java APIs whose use a script step should review.
const REVIEW_MARKERS: &[&str] = &[
    "Packages.",
    "java.",
    "javax.",
    "importPackage",
    "importClass",
    "JavaAdapter",
    "DatabaseConnectionFactory",
    "SMTPConnectionFactory",
    "FileUtil",
    "SerializerFactory",
    "router.",
    "VMRouter",
    "AttachmentUtil",
    "addAttachment",
    "getAttachments",
    "ChannelUtil",
    "destinationSet",
    "ContextFactory",
    "alerts.",
    "new XML(",
    "XMLList(",
    "Namespace(",
];

static XML_LITERAL: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"(?:=|return|\(|,)\s*<[A-Za-z]").ok());

fn review(script: &str) -> Vec<&'static str> {
    let mut found: Vec<&'static str> = REVIEW_MARKERS
        .iter()
        .copied()
        .filter(|marker| script.contains(marker))
        .collect();
    if XML_LITERAL
        .as_ref()
        .is_some_and(|regex| regex.is_match(script))
    {
        found.push("E4X XML literals");
    }
    found
}

/// The step elements of a filter or transformer, in Mirth's order.
fn elements(container: Option<&Element>) -> Vec<&Element> {
    let Some(elements) = container.and_then(|c| c.child("elements")) else {
        return Vec::new();
    };
    let mut list: Vec<(u64, usize, &Element)> = elements
        .children
        .iter()
        .enumerate()
        .map(|(index, element)| {
            let order = element
                .number("sequenceNumber")
                .unwrap_or(u64::try_from(index).unwrap_or(u64::MAX));
            (order, index, element)
        })
        .collect();
    list.sort_by_key(|(order, index, _)| (*order, *index));
    list.into_iter().map(|(_, _, element)| element).collect()
}

fn short_class(element: &Element) -> &str {
    element.name.rsplit('.').next().unwrap_or(&element.name)
}

fn label(scope: &Scope<'_>, what: &str, element: &Element) -> String {
    let name = element.value("name").unwrap_or("unnamed");
    format!(
        "{} {what} \"{name}\" ({})",
        scope.label,
        short_class(element)
    )
}

/// Where a script step comes from, for the script's first line and the
/// report.
struct Origin<'a> {
    /// The first comment line of the script.
    heading: &'a str,
    /// The report element.
    label: &'a str,
    /// The location in the export.
    location: &'a str,
    /// More detail for the report.
    extra: Option<&'a str>,
    /// Whether to report the step as approximated regardless of content.
    approximate: bool,
}

/// Builds a `script` step and reports it.
fn script_step(body: &str, origin: &Origin<'_>, scope: &Scope<'_>, notes: &mut Notes<'_>) -> Yaml {
    let body = normalize_newlines(body);
    let (source, used) = with_templates(&body, scope.templates);
    let source = format!("// {}\n{source}", origin.heading);
    let mut detail = "runs in the OXIM `script` step in Mirth compatibility mode".to_owned();
    if let Some(extra) = origin.extra {
        detail.push_str("; ");
        detail.push_str(extra);
    }
    if !used.is_empty() {
        detail.push_str(&format!("; code templates included: {}", used.join(", ")));
    }
    let markers = review(&source);
    if markers.is_empty() && !origin.approximate {
        notes.converted(origin.label, origin.location, detail);
    } else {
        if !markers.is_empty() {
            detail.push_str(&format!(
                "; review the script: it uses {}, which OXIM scripts may not provide",
                markers.join(", ")
            ));
        }
        notes.approximated(origin.label, origin.location, detail);
    }
    Yaml::map([
        ("type", Yaml::str("script")),
        ("mirth", Yaml::Bool(true)),
        ("source", Yaml::str(source)),
    ])
}

/// A converted filter rule.
enum RuleKind {
    /// A declarative rule: its `condition` leaf and its standalone step.
    Leaf { condition: Yaml, step: Yaml },
    /// A JavaScript rule: the body of a function that returns a boolean.
    Script { body: String },
}

struct Rule<'e> {
    or: bool,
    kind: RuleKind,
    label: String,
    element: &'e Element,
}

/// The JavaScript the rule builder would generate: a boolean expression.
fn rule_expression(field: &str, condition: &str, values: &[String]) -> Option<String> {
    let f = format!("String({field})");
    let join = |op: &str, template: &dyn Fn(&str) -> String, empty: &str| {
        if values.is_empty() {
            empty.to_owned()
        } else {
            values
                .iter()
                .map(|value| template(value))
                .collect::<Vec<_>>()
                .join(op)
        }
    };
    let expression = match condition {
        "EXISTS" => format!("{f}.length > 0"),
        "NOT_EXIST" => format!("{f}.length == 0"),
        "EQUALS" => join(" || ", &|v| format!("{f} == {v}"), "false"),
        "NOT_EQUAL" => join(" && ", &|v| format!("{f} != {v}"), "true"),
        "CONTAINS" => join(" || ", &|v| format!("{f}.indexOf({v}) != -1"), "false"),
        "NOT_CONTAIN" => join(" && ", &|v| format!("{f}.indexOf({v}) == -1"), "true"),
        _ => return None,
    };
    Some(format!("({expression})"))
}

fn contains_pattern(values: &[String]) -> String {
    values
        .iter()
        .map(|value| regex::escape(value))
        .collect::<Vec<_>>()
        .join("|")
}

/// A rule-builder rule as a declarative leaf, when the field is an HL7 v2
/// field and every value a string literal.
fn rule_leaf(path: &str, condition: &str, values: &[String]) -> Option<(Yaml, Yaml)> {
    let path_entry = || ("path", Yaml::str(path));
    let one = values.len() == 1;
    Some(match condition {
        "EXISTS" | "NOT_EXIST" => {
            let exists = condition == "EXISTS";
            let mut step = vec![("type", Yaml::str("path-exists")), path_entry()];
            if !exists {
                step.push(("negate", Yaml::Bool(true)));
            }
            (
                Yaml::map([path_entry(), ("exists", Yaml::Bool(exists))]),
                Yaml::map(step),
            )
        }
        "EQUALS" | "NOT_EQUAL" if !values.is_empty() => {
            let negate = condition == "NOT_EQUAL";
            let (test, kind, key, value) = if one {
                (
                    if negate { "not_equals" } else { "equals" },
                    "path-equals",
                    "value",
                    Yaml::str(values[0].clone()),
                )
            } else {
                (
                    if negate { "not_in" } else { "in" },
                    "path-in",
                    "values",
                    Yaml::strings(values.iter().cloned()),
                )
            };
            let mut step = vec![
                ("type", Yaml::str(kind)),
                path_entry(),
                (key, value.clone()),
            ];
            if negate {
                step.push(("negate", Yaml::Bool(true)));
            }
            (Yaml::map([path_entry(), (test, value)]), Yaml::map(step))
        }
        "CONTAINS" | "NOT_CONTAIN" if !values.is_empty() => {
            let leaf = Yaml::map([
                path_entry(),
                ("matches", Yaml::str(contains_pattern(values))),
            ]);
            let condition = if condition == "CONTAINS" {
                leaf
            } else {
                Yaml::map([("not", leaf)])
            };
            let mut step = vec![("type".to_owned(), Yaml::str("condition"))];
            if let Yaml::Map(entries) = &condition {
                step.extend(entries.iter().cloned());
            }
            (condition, Yaml::Map(step))
        }
        _ => return None,
    })
}

fn rule_of<'e>(element: &'e Element, scope: &Scope<'_>, notes: &mut Notes<'_>) -> Option<Rule<'e>> {
    let or = element.value("operator") == Some("OR");
    let label = label(scope, "filter rule", element);
    match short_class(element) {
        "RuleBuilderRule" => {
            let field = element.value("field").unwrap_or("''");
            let condition = element.value("condition").unwrap_or("EXISTS");
            let values: Vec<String> = element
                .child("values")
                .map(|list| list.strings().iter().map(|v| v.trim().to_owned()).collect())
                .unwrap_or_default();
            let Some(expression) = rule_expression(field, condition, &values) else {
                notes.unsupported(
                    &label,
                    &element.location,
                    format!("the rule condition {condition} is not known; the rule was left out"),
                );
                return None;
            };
            let literals: Option<Vec<String>> = values.iter().map(|v| js_string(v)).collect();
            let leaf = scope
                .hl7
                .then(|| hl7_path(field, "msg"))
                .flatten()
                .zip(literals)
                .and_then(|(path, literals)| rule_leaf(&path, condition, &literals));
            let kind = match leaf {
                Some((condition, step)) => RuleKind::Leaf { condition, step },
                None => RuleKind::Script {
                    body: format!("return {expression};"),
                },
            };
            Some(Rule {
                or,
                kind,
                label,
                element,
            })
        }
        "JavaScriptRule" => {
            let body = normalize_newlines(element.script("script").unwrap_or("return true;"));
            Some(Rule {
                or,
                kind: RuleKind::Script { body },
                label,
                element,
            })
        }
        other => {
            notes.unsupported(
                &label,
                &element.location,
                format!("{other} filter rules are not supported; the rule was left out"),
            );
            None
        }
    }
}

/// Converts a Mirth filter into OXIM filters. Every OXIM filter must
/// accept a message, which matches rules joined with AND; rules joined with
/// OR become one `condition` filter (declarative rules) or one `script`
/// filter.
pub(crate) fn filters(
    filter: Option<&Element>,
    scope: &Scope<'_>,
    notes: &mut Notes<'_>,
) -> Vec<Yaml> {
    let mut rules = Vec::new();
    for element in elements(filter) {
        if element.flag("enabled") == Some(false) {
            notes.converted(
                &label(scope, "filter rule", element),
                &element.location,
                "disabled in Mirth; left out",
            );
            continue;
        }
        if let Some(rule) = rule_of(element, scope, notes) {
            rules.push(rule);
        }
    }
    if rules.is_empty() {
        return Vec::new();
    }
    let mut groups: Vec<Vec<Rule<'_>>> = Vec::new();
    for (index, rule) in rules.into_iter().enumerate() {
        match groups.last_mut() {
            Some(group) if index == 0 || !rule.or => group.push(rule),
            _ => groups.push(vec![rule]),
        }
    }
    if groups.len() == 1 {
        let group = groups.pop().unwrap_or_default();
        return group
            .into_iter()
            .map(|rule| match rule.kind {
                RuleKind::Leaf { step, .. } => {
                    notes.converted(
                        &rule.label,
                        &rule.element.location,
                        "declarative OXIM filter",
                    );
                    step
                }
                RuleKind::Script { body, .. } => script_step(
                    &body,
                    &Origin {
                        heading: &format!(
                            "Mirth filter rule: {}",
                            rule.element.value("name").unwrap_or("unnamed")
                        ),
                        label: &rule.label,
                        location: &rule.element.location,
                        extra: None,
                        approximate: false,
                    },
                    scope,
                    notes,
                ),
            })
            .collect();
    }
    let declarative = groups
        .iter()
        .flatten()
        .all(|rule| matches!(rule.kind, RuleKind::Leaf { .. }));
    if declarative {
        let any: Vec<Yaml> = groups
            .iter()
            .map(|group| {
                let leaves: Vec<Yaml> = group
                    .iter()
                    .filter_map(|rule| match &rule.kind {
                        RuleKind::Leaf { condition, .. } => Some(condition.clone()),
                        RuleKind::Script { .. } => None,
                    })
                    .collect();
                if leaves.len() == 1 {
                    leaves.into_iter().next().unwrap_or(Yaml::Bool(true))
                } else {
                    Yaml::map([("all", Yaml::List(leaves))])
                }
            })
            .collect();
        for rule in groups.iter().flatten() {
            notes.converted(
                &rule.label,
                &rule.element.location,
                "part of one `condition` filter that joins the rules with AND and OR as Mirth does",
            );
        }
        return vec![Yaml::map([
            ("type", Yaml::str("condition")),
            ("any", Yaml::List(any)),
        ])];
    }
    // Mixed rules: one script with a function per rule, joined like Mirth.
    let mut body = String::new();
    let mut joined = Vec::new();
    let mut index = 0usize;
    let mut labels = Vec::new();
    for group in &groups {
        let mut terms = Vec::new();
        for rule in group {
            index += 1;
            let function = match &rule.kind {
                RuleKind::Script { body, .. } => body.clone(),
                RuleKind::Leaf { .. } => String::new(),
            };
            let function = if function.is_empty() {
                // Declarative rules are rebuilt from their Mirth expression.
                let field = rule.element.value("field").unwrap_or("''");
                let condition = rule.element.value("condition").unwrap_or("EXISTS");
                let values: Vec<String> = rule
                    .element
                    .child("values")
                    .map(|list| list.strings().iter().map(|v| v.trim().to_owned()).collect())
                    .unwrap_or_default();
                format!(
                    "return {};",
                    rule_expression(field, condition, &values).unwrap_or_else(|| "true".into())
                )
            } else {
                function
            };
            let indented: Vec<String> = function
                .lines()
                .map(|line| {
                    if line.is_empty() {
                        String::new()
                    } else {
                        format!("    {line}")
                    }
                })
                .collect();
            body.push_str(&format!(
                "// {}\nfunction rule{index}() {{\n{}\n}}\n\n",
                rule.label,
                indented.join("\n")
            ));
            terms.push(format!("rule{index}()"));
            labels.push((rule.label.clone(), rule.element.location.clone()));
        }
        joined.push(terms.join(" && "));
    }
    body.push_str(&format!("return {};", joined.join(" || ")));
    let location = filter.map_or("", |f| f.location.as_str());
    let step = script_step(
        &body,
        &Origin {
            heading: "Mirth filter rules joined with AND and OR",
            label: &format!("{} filter", scope.label),
            location,
            extra: Some("the rules are joined in one script because they mix OR with JavaScript"),
            approximate: false,
        },
        scope,
        notes,
    );
    for (label, location) in labels {
        notes.converted(&label, &location, "part of the combined filter script");
    }
    vec![step]
}

/// Where a mapper value comes from.
enum ValueSource {
    Path(String),
    Literal(String),
    Variable(String),
}

fn value_source(mapping: &str, scope: &Scope<'_>) -> Option<ValueSource> {
    let mapping = mapping.trim();
    if mapping.is_empty() {
        return Some(ValueSource::Literal(String::new()));
    }
    if scope.hl7
        && let Some(path) = hl7_path(mapping, "msg")
    {
        return Some(ValueSource::Path(path));
    }
    if let Some(text) = js_string(mapping) {
        return Some(ValueSource::Literal(text));
    }
    js_variable(mapping).map(ValueSource::Variable)
}

/// A template for the value with an optional default; `None` when the
/// default cannot be written in a template.
fn value_template(source: &ValueSource, default: &str) -> Option<String> {
    if default.contains(['{', '}', '|']) {
        return None;
    }
    let with_default = |inner: &str| {
        if default.is_empty() {
            format!("{{{inner}}}")
        } else {
            format!("{{{inner}|{default}}}")
        }
    };
    Some(match source {
        ValueSource::Path(path) => with_default(path),
        ValueSource::Variable(name) => with_default(&format!("${name}")),
        ValueSource::Literal(text) if text.is_empty() => template_text(default),
        ValueSource::Literal(text) => template_text(text),
    })
}

fn replacements(step: &Element) -> Vec<(String, String)> {
    step.child("replacements")
        .map(|list| {
            list.children_named("entry")
                .filter_map(|entry| {
                    let strings: Vec<&Element> = entry.children_named("string").collect();
                    match strings.as_slice() {
                        [key, value] => {
                            Some((key.raw_text().to_owned(), value.raw_text().to_owned()))
                        }
                        _ => None,
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A `map` operation for a mapper step, when it only copies a value into a
/// channel variable.
fn mapper_operation(step: &Element, scope: &Scope<'_>) -> Option<(Yaml, String)> {
    let variable = step.value("variable").filter(|v| is_identifier(v))?;
    if !matches!(
        step.value("scope").unwrap_or("CHANNEL"),
        "CHANNEL" | "CONNECTOR"
    ) {
        return None;
    }
    if !replacements(step).is_empty() {
        return None;
    }
    let source = value_source(step.script("mapping").unwrap_or_default(), scope)?;
    let default = step.script("defaultValue").unwrap_or_default();
    let operation = match (&source, default.is_empty()) {
        (ValueSource::Path(path), true) => Yaml::map([
            ("path", Yaml::str(path.clone())),
            ("as", Yaml::str(variable)),
        ]),
        _ => Yaml::map([
            ("value", Yaml::str(value_template(&source, default)?)),
            ("as", Yaml::str(variable)),
        ]),
    };
    Some((
        Yaml::map([("store", operation)]),
        format!(
            "`map` operation storing the message variable `{variable}` (channelMap in scripts)"
        ),
    ))
}

/// A `map` operation for a message builder step, when it writes a value
/// into an HL7 v2 field of the message.
fn builder_operation(step: &Element, scope: &Scope<'_>) -> Option<(Yaml, String)> {
    if !scope.hl7 || !replacements(step).is_empty() {
        return None;
    }
    let target = hl7_path(step.value("messageSegment")?, "msg")?;
    let source = value_source(step.script("mapping").unwrap_or_default(), scope)?;
    let default = step.script("defaultValue").unwrap_or_default();
    let operation = match (&source, default.is_empty()) {
        (ValueSource::Path(path), true) => Yaml::map([(
            "copy",
            Yaml::map([
                ("from", Yaml::str(path.clone())),
                ("to", Yaml::str(target.clone())),
            ]),
        )]),
        _ => Yaml::map([(
            "set",
            Yaml::map([
                ("path", Yaml::str(target.clone())),
                ("value", Yaml::str(value_template(&source, default)?)),
            ]),
        )]),
    };
    Some((operation, format!("`map` operation writing {target}")))
}

/// The JavaScript Mirth generates for a mapper or message builder step.
fn mapping_script(step: &Element, assign: &str) -> String {
    let mapping = step
        .script("mapping")
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .unwrap_or("''");
    let default = js_quote(step.script("defaultValue").unwrap_or_default());
    let mut out = format!(
        "var mapping;\ntry {{\n    mapping = {mapping};\n}} catch (e) {{\n    mapping = '';\n}}\n\
         mapping = (mapping === undefined || mapping === null) ? '' : String(mapping);\n\
         if (mapping.length == 0) {{\n    mapping = {default};\n}}\n"
    );
    for (key, value) in replacements(step) {
        out.push_str(&format!(
            "mapping = mapping.replace(new RegExp({}, 'g'), {});\n",
            key.trim(),
            value.trim()
        ));
    }
    out.push_str(assign);
    out
}

fn map_name(scope: &str) -> &'static str {
    match scope {
        "CONNECTOR" => "connectorMap",
        "GLOBAL" => "globalMap",
        "GLOBAL_CHANNEL" => "globalChannelMap",
        "RESPONSE" => "responseMap",
        _ => "channelMap",
    }
}

/// Converts a Mirth transformer into OXIM transformers. Consecutive steps
/// that become `map` operations share one `map` transformer.
pub(crate) fn transformers(
    transformer: Option<&Element>,
    scope: &Scope<'_>,
    notes: &mut Notes<'_>,
) -> Vec<Yaml> {
    let mut out = Vec::new();
    let mut operations: Vec<Yaml> = Vec::new();
    let flush = |operations: &mut Vec<Yaml>, out: &mut Vec<Yaml>| {
        if !operations.is_empty() {
            out.push(Yaml::map([
                ("type", Yaml::str("map")),
                ("operations", Yaml::List(std::mem::take(operations))),
            ]));
        }
    };
    for step in elements(transformer) {
        let step_label = label(scope, "transformer step", step);
        if step.flag("enabled") == Some(false) {
            notes.converted(&step_label, &step.location, "disabled in Mirth; left out");
            continue;
        }
        let heading = format!(
            "Mirth transformer step: {}",
            step.value("name").unwrap_or("unnamed")
        );
        match short_class(step) {
            "JavaScriptStep" => {
                flush(&mut operations, &mut out);
                out.push(script_step(
                    step.script("script").unwrap_or_default(),
                    &Origin {
                        heading: &heading,
                        label: &step_label,
                        location: &step.location,
                        extra: None,
                        approximate: false,
                    },
                    scope,
                    notes,
                ));
            }
            class @ ("MapperStep" | "MessageBuilderStep") => {
                let converted = if class == "MapperStep" {
                    mapper_operation(step, scope)
                } else {
                    builder_operation(step, scope)
                };
                if let Some((operation, detail)) = converted {
                    notes.converted(&step_label, &step.location, detail);
                    operations.push(operation);
                    continue;
                }
                flush(&mut operations, &mut out);
                let (assign, extra) = if class == "MapperStep" {
                    let map_scope = step.value("scope").unwrap_or("CHANNEL");
                    let variable = step.value("variable").unwrap_or("value");
                    let extra =
                        matches!(map_scope, "GLOBAL" | "GLOBAL_CHANNEL" | "RESPONSE").then(|| {
                            format!(
                                "the value goes to Mirth's {}, which is shared between messages; \
                                 verify the behavior",
                                map_name(map_scope)
                            )
                        });
                    (
                        format!(
                            "{}.put({}, mapping);",
                            map_name(map_scope),
                            js_quote(variable)
                        ),
                        extra,
                    )
                } else {
                    let target = step.value("messageSegment").unwrap_or("tmp");
                    (format!("{target} = mapping;"), None)
                };
                let script = mapping_script(step, &assign);
                out.push(script_step(
                    &script,
                    &Origin {
                        heading: &heading,
                        label: &step_label,
                        location: &step.location,
                        extra: Some(
                            extra
                                .as_deref()
                                .unwrap_or("rebuilt as the JavaScript Mirth generates"),
                        ),
                        approximate: extra.is_some(),
                    },
                    scope,
                    notes,
                ));
            }
            "XsltStep" => notes.unsupported(
                &step_label,
                &step.location,
                "XSLT steps are not supported; the step was left out",
            ),
            other => notes.unsupported(
                &step_label,
                &step.location,
                format!("{other} transformer steps are not supported; the step was left out"),
            ),
        }
    }
    flush(&mut operations, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_rule_expressions_like_the_rule_builder() {
        let values = vec!["'ADT'".to_owned(), "'ORU'".to_owned()];
        assert_eq!(
            rule_expression(
                "msg['MSH']['MSH.9']['MSH.9.1'].toString()",
                "EQUALS",
                &values
            )
            .unwrap(),
            "(String(msg['MSH']['MSH.9']['MSH.9.1'].toString()) == 'ADT' || \
             String(msg['MSH']['MSH.9']['MSH.9.1'].toString()) == 'ORU')"
        );
        assert_eq!(rule_expression("x", "NOT_EQUAL", &[]).unwrap(), "(true)");
        assert!(rule_expression("x", "MATCHES", &values).is_none());
        assert_eq!(contains_pattern(&["a.b".into(), "c".into()]), r"a\.b|c");
        assert_eq!(review("var x = new XML('<a/>');"), ["new XML("]);
        assert_eq!(review("var x = <a/>;"), ["E4X XML literals"]);
        assert!(review("if (a < b) { x = 1; }").is_empty());
    }
}
