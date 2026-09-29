//! The `map` transformer: an ordered list of edit operations.

use std::sync::Arc;

use oxim_core::{EngineError, MessageContext, StepConfig, StepError, Transformer};
use regex::{Regex, RegexBuilder};
use serde_json::Value;

use crate::condition::Condition;
use crate::date::DateFormat;
use crate::environment::TransformEnvironment;
use crate::path::{PathSpec, occurrences};
use crate::settings::{Obj, config_error};
use crate::table::CodeTable;
use crate::template::Template;

/// What `lookup` does when a code is not in the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OnMissing {
    Keep,
    Empty,
    Error,
}

/// What `date` does with a value it cannot read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OnInvalid {
    Keep,
    Empty,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Column {
    To,
    Display,
    System,
}

#[derive(Debug, Clone)]
enum Action {
    Set {
        path: PathSpec,
        value: Template,
    },
    Copy {
        from: PathSpec,
        to: PathSpec,
    },
    Clear {
        path: PathSpec,
    },
    Trim {
        path: PathSpec,
    },
    Upper {
        path: PathSpec,
    },
    Lower {
        path: PathSpec,
    },
    Replace {
        path: PathSpec,
        pattern: Regex,
        with: String,
    },
    Pad {
        path: PathSpec,
        length: usize,
        fill: char,
        side: Side,
    },
    Substring {
        path: PathSpec,
        start: usize,
        length: Option<usize>,
    },
    Date {
        path: PathSpec,
        from: DateFormat,
        to: DateFormat,
        on_invalid: OnInvalid,
    },
    Lookup {
        table: Arc<CodeTable>,
        from: PathSpec,
        to: PathSpec,
        column: Column,
        context: Option<Template>,
        default: Option<Template>,
        on_missing: OnMissing,
    },
    Store {
        value: Template,
        variable: String,
    },
}

#[derive(Debug, Clone)]
struct Operation {
    when: Option<Condition>,
    action: Action,
    /// The wildcard the operation iterates, if any of its paths has one.
    wildcard: Option<String>,
}

/// Applies an ordered list of operations to the parsed document.
///
/// ```yaml
/// transformers:
///   - type: map
///     operations:
///       - set: {path: MSH-3, value: OXIM}
///       - set: {path: MSH-10, value: "{message.id}"}
///       - copy: {from: PID-3.1, to: PID-2}
///       - clear: {path: PID-19}
///       - trim: {path: PID-5.1}
///       - upper: {path: PID-5.1}
///       - lower: {path: PID-5.2}
///       - replace: {path: PID-3.1, pattern: "^0+", with: ""}
///       - pad: {path: OBR-3, length: 10, char: "0", side: left}
///       - substring: {path: PID-5.1, start: 0, length: 20}
///       - date: {path: PID-7, from: hl7, to: "%d.%m.%Y"}
///       - lookup: {table: tables/tests.csv, from: "OBX[*]-3.1", to: "OBX[*]-3.1", on_missing: keep}
///       - store: {path: PID-3.1, as: patient_id}
///       - when: {path: MSH-9.1, equals: ORU}
///         set: {path: MSH-5, value: "LIS-{$patient_id}"}
/// ```
///
/// Every operation has exactly one action and an optional `when` condition.
/// Operations whose paths contain `[*]` run once per occurrence (every OBX
/// segment, every ASTM R record, ...); `when` then tests the current
/// occurrence. All wildcard paths of one operation must share the same
/// segment or element. Operations that read a missing value leave the
/// document unchanged, except `copy`, which writes an empty value, and
/// `lookup`, which follows `on_missing`.
#[derive(Debug, Clone)]
pub struct MapTransformer {
    operations: Vec<Operation>,
}

const ACTIONS: &[&str] = &[
    "set",
    "copy",
    "clear",
    "trim",
    "upper",
    "lower",
    "replace",
    "pad",
    "substring",
    "date",
    "lookup",
    "store",
];

impl MapTransformer {
    /// Builds the transformer from its step configuration.
    pub fn from_step(
        step: &StepConfig,
        environment: &TransformEnvironment,
    ) -> Result<Self, EngineError> {
        let root = Obj::from_map("map", &step.settings);
        root.only(&["operations"])?;
        let Some(Value::Array(items)) = root.value("operations") else {
            return Err(config_error("map", "needs an `operations` list"));
        };
        let operations = items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                Operation::parse(&format!("map.operations[{index}]"), item, environment)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { operations })
    }
}

impl Transformer for MapTransformer {
    fn apply(&self, context: &mut MessageContext) -> Result<(), StepError> {
        for (index, operation) in self.operations.iter().enumerate() {
            operation
                .apply(context)
                .map_err(|e| StepError::new(format!("map.operations[{index}]"), e.to_string()))?;
        }
        Ok(())
    }
}

fn path(at: &str, obj: Obj<'_>, key: &str) -> Result<PathSpec, EngineError> {
    PathSpec::parse(&format!("{at}.{key}"), &obj.required_text(key)?)
}

impl Operation {
    fn parse(
        at: &str,
        value: &Value,
        environment: &TransformEnvironment,
    ) -> Result<Self, EngineError> {
        let obj = Obj::new(at, value)?;
        let mut allowed = ACTIONS.to_vec();
        allowed.push("when");
        obj.only(&allowed)?;
        let actions: Vec<&str> = ACTIONS.iter().copied().filter(|key| obj.has(key)).collect();
        let [name] = actions.as_slice() else {
            return Err(config_error(
                at,
                format!("an operation needs exactly one of {}", ACTIONS.join(", ")),
            ));
        };
        let when = obj
            .value("when")
            .map(|condition| Condition::parse_at(&format!("{at}.when"), condition))
            .transpose()?;
        let at = format!("{at}.{name}");
        let args = obj
            .value(name)
            .ok_or_else(|| config_error(&at, "missing settings"))?;
        let args = Obj::new(&at, args)?;
        let action = Self::action(&at, name, args, environment)?;

        let mut prefixes: Vec<String> = action_prefixes(&action);
        if matches!(action, Action::Store { .. }) && !prefixes.is_empty() {
            return Err(config_error(
                &at,
                "store cannot read a [*] path; it keeps a single value",
            ));
        }
        if let Some(condition) = &when {
            prefixes.extend(condition.wildcard_prefixes());
        }
        prefixes.dedup();
        let wildcard = match prefixes.as_slice() {
            [] => None,
            [prefix] => Some(prefix.clone()),
            _ if prefixes.iter().all(|p| *p == prefixes[0]) => Some(prefixes[0].clone()),
            _ => {
                return Err(config_error(
                    &at,
                    format!(
                        "all [*] paths of one operation must share the same prefix, found {}",
                        prefixes.join(", ")
                    ),
                ));
            }
        };
        Ok(Self {
            when,
            action,
            wildcard,
        })
    }

    fn action(
        at: &str,
        name: &str,
        args: Obj<'_>,
        environment: &TransformEnvironment,
    ) -> Result<Action, EngineError> {
        let one_path = |args: Obj<'_>| -> Result<PathSpec, EngineError> {
            args.only(&["path"])?;
            path(at, args, "path")
        };
        Ok(match name {
            "set" => {
                args.only(&["path", "value"])?;
                Action::Set {
                    path: path(at, args, "path")?,
                    value: Template::parse_at(at, &args.text("value")?.unwrap_or_default())?,
                }
            }
            "copy" => {
                args.only(&["from", "to"])?;
                Action::Copy {
                    from: path(at, args, "from")?,
                    to: path(at, args, "to")?,
                }
            }
            "clear" => Action::Clear {
                path: one_path(args)?,
            },
            "trim" => Action::Trim {
                path: one_path(args)?,
            },
            "upper" => Action::Upper {
                path: one_path(args)?,
            },
            "lower" => Action::Lower {
                path: one_path(args)?,
            },
            "replace" => {
                args.only(&["path", "pattern", "with", "case_insensitive"])?;
                let pattern = args.required_text("pattern")?;
                Action::Replace {
                    path: path(at, args, "path")?,
                    pattern: RegexBuilder::new(&pattern)
                        .case_insensitive(args.bool("case_insensitive")?.unwrap_or(false))
                        .size_limit(1 << 20)
                        .build()
                        .map_err(|e| {
                            config_error(at, format!("invalid regular expression: {e}"))
                        })?,
                    with: args.text("with")?.unwrap_or_default(),
                }
            }
            "pad" => {
                args.only(&["path", "length", "char", "side"])?;
                let fill = args.text("char")?.unwrap_or_else(|| " ".into());
                let mut chars = fill.chars();
                let (Some(fill), None) = (chars.next(), chars.next()) else {
                    return Err(config_error(at, "char must be a single character"));
                };
                let side = match args.text("side")?.as_deref() {
                    None | Some("left") => Side::Left,
                    Some("right") => Side::Right,
                    Some(other) => {
                        return Err(config_error(
                            at,
                            format!("side must be left or right, not {other:?}"),
                        ));
                    }
                };
                Action::Pad {
                    path: path(at, args, "path")?,
                    length: args
                        .usize("length")?
                        .filter(|n| *n <= 10_000)
                        .ok_or_else(|| {
                            config_error(at, "length must be a whole number up to 10000")
                        })?,
                    fill,
                    side,
                }
            }
            "substring" => {
                args.only(&["path", "start", "length"])?;
                Action::Substring {
                    path: path(at, args, "path")?,
                    start: args.usize("start")?.unwrap_or(0),
                    length: args.usize("length")?,
                }
            }
            "date" => {
                args.only(&["path", "from", "to", "on_invalid"])?;
                Action::Date {
                    path: path(at, args, "path")?,
                    from: DateFormat::parse(at, &args.required_text("from")?)?,
                    to: DateFormat::parse(at, &args.required_text("to")?)?,
                    on_invalid: match args.text("on_invalid")?.as_deref() {
                        None | Some("error") => OnInvalid::Error,
                        Some("keep") => OnInvalid::Keep,
                        Some("empty") => OnInvalid::Empty,
                        Some(other) => {
                            return Err(config_error(
                                at,
                                format!("on_invalid must be error, keep or empty, not {other:?}"),
                            ));
                        }
                    },
                }
            }
            "lookup" => {
                args.only(&[
                    "table",
                    "from",
                    "to",
                    "column",
                    "context",
                    "default",
                    "on_missing",
                    "case_insensitive",
                ])?;
                let table = environment.table(
                    &args.required_text("table")?,
                    args.bool("case_insensitive")?.unwrap_or(false),
                )?;
                let from = path(at, args, "from")?;
                let to = match args.text("to")? {
                    Some(to) => PathSpec::parse(at, &to)?,
                    None => from.clone(),
                };
                Action::Lookup {
                    table,
                    from,
                    to,
                    column: match args.text("column")?.as_deref() {
                        None | Some("to") => Column::To,
                        Some("display") => Column::Display,
                        Some("system") => Column::System,
                        Some(other) => {
                            return Err(config_error(
                                at,
                                format!("column must be to, display or system, not {other:?}"),
                            ));
                        }
                    },
                    context: args
                        .text("context")?
                        .map(|text| Template::parse_at(at, &text))
                        .transpose()?,
                    default: args
                        .text("default")?
                        .map(|text| Template::parse_at(at, &text))
                        .transpose()?,
                    on_missing: match args.text("on_missing")?.as_deref() {
                        None | Some("keep") => OnMissing::Keep,
                        Some("empty") => OnMissing::Empty,
                        Some("error") => OnMissing::Error,
                        Some(other) => {
                            return Err(config_error(
                                at,
                                format!("on_missing must be keep, empty or error, not {other:?}"),
                            ));
                        }
                    },
                }
            }
            _ => {
                args.only(&["path", "value", "as"])?;
                let value = match (args.text("path")?, args.text("value")?) {
                    (Some(path), None) => Template::parse_at(at, &format!("{{{path}}}"))?,
                    (None, Some(value)) => Template::parse_at(at, &value)?,
                    _ => return Err(config_error(at, "store needs exactly one of path or value")),
                };
                Action::Store {
                    value,
                    variable: args.required_text("as")?,
                }
            }
        })
    }

    fn apply(&self, context: &mut MessageContext) -> Result<(), StepError> {
        match &self.wildcard {
            None => self.apply_once(context, None),
            Some(prefix) => {
                let count = occurrences(&context.document, prefix)?;
                for n in 1..=count {
                    self.apply_once(context, Some(n))?;
                }
                Ok(())
            }
        }
    }

    fn apply_once(
        &self,
        context: &mut MessageContext,
        occurrence: Option<usize>,
    ) -> Result<(), StepError> {
        if let Some(condition) = &self.when
            && !condition.evaluate(context, occurrence)?
        {
            return Ok(());
        }
        let get = |context: &MessageContext, path: &PathSpec| {
            context.document.get(&path.resolve(occurrence))
        };
        let set = |context: &mut MessageContext, path: &PathSpec, value: &str| {
            context.document.set(&path.resolve(occurrence), value)
        };
        let edit =
            |context: &mut MessageContext, path: &PathSpec, change: &dyn Fn(&str) -> String| {
                match get(context, path)? {
                    Some(value) => {
                        let changed = change(&value);
                        if changed == value {
                            Ok(())
                        } else {
                            set(context, path, &changed)
                        }
                    }
                    None => Ok(()),
                }
            };
        match &self.action {
            Action::Set { path, value } => {
                let text = value.render(context, occurrence)?;
                set(context, path, &text)
            }
            Action::Copy { from, to } => {
                let value = get(context, from)?.unwrap_or_default();
                set(context, to, &value)
            }
            Action::Clear { path } => {
                if get(context, path)?.is_some_and(|value| !value.is_empty()) {
                    set(context, path, "")
                } else {
                    Ok(())
                }
            }
            Action::Trim { path } => edit(context, path, &|value| value.trim().to_owned()),
            Action::Upper { path } => edit(context, path, &str::to_uppercase),
            Action::Lower { path } => edit(context, path, &str::to_lowercase),
            Action::Replace {
                path,
                pattern,
                with,
            } => edit(context, path, &|value| {
                pattern.replace_all(value, with.as_str()).into_owned()
            }),
            Action::Pad {
                path,
                length,
                fill,
                side,
            } => edit(context, path, &|value| {
                let missing = length.saturating_sub(value.chars().count());
                let padding: String = std::iter::repeat_n(*fill, missing).collect();
                match side {
                    Side::Left => format!("{padding}{value}"),
                    Side::Right => format!("{value}{padding}"),
                }
            }),
            Action::Substring {
                path,
                start,
                length,
            } => edit(context, path, &|value| {
                let chars = value.chars().skip(*start);
                match length {
                    Some(length) => chars.take(*length).collect(),
                    None => chars.collect(),
                }
            }),
            Action::Date {
                path,
                from,
                to,
                on_invalid,
            } => {
                let Some(value) = get(context, path)?.filter(|value| !value.trim().is_empty())
                else {
                    return Ok(());
                };
                let converted = from.read(&value).and_then(|date| to.write(&date));
                match (converted, on_invalid) {
                    (Ok(text), _) => set(context, path, &text),
                    (Err(_), OnInvalid::Keep) => Ok(()),
                    (Err(_), OnInvalid::Empty) => set(context, path, ""),
                    (Err(e), OnInvalid::Error) => Err(StepError::new("date", e)),
                }
            }
            Action::Lookup {
                table,
                from,
                to,
                column,
                context: scope,
                default,
                on_missing,
            } => {
                let Some(code) = get(context, from)?.filter(|code| !code.trim().is_empty()) else {
                    return Ok(());
                };
                let scope = scope
                    .as_ref()
                    .map(|t| t.render(context, occurrence))
                    .transpose()?;
                match table.lookup(scope.as_deref(), &code) {
                    Some(entry) => {
                        let value = match column {
                            Column::To => Some(entry.to.as_str()),
                            Column::Display => entry.display.as_deref(),
                            Column::System => entry.system.as_deref(),
                        }
                        .unwrap_or_default()
                        .to_owned();
                        set(context, to, &value)
                    }
                    None => {
                        if let Some(default) = default {
                            let value = default.render(context, occurrence)?;
                            return set(context, to, &value);
                        }
                        match on_missing {
                            OnMissing::Keep => set(context, to, &code),
                            OnMissing::Empty => set(context, to, ""),
                            OnMissing::Error => Err(StepError::new(
                                "lookup",
                                format!("code {code:?} is not in the table"),
                            )),
                        }
                    }
                }
            }
            Action::Store { value, variable } => {
                let text = value.render(context, occurrence)?;
                context.variables.insert(variable.clone(), text);
                Ok(())
            }
        }
    }
}

fn action_prefixes(action: &Action) -> Vec<String> {
    let paths: Vec<&PathSpec> = match action {
        Action::Set { path, .. }
        | Action::Clear { path }
        | Action::Trim { path }
        | Action::Upper { path }
        | Action::Lower { path }
        | Action::Replace { path, .. }
        | Action::Pad { path, .. }
        | Action::Substring { path, .. }
        | Action::Date { path, .. } => vec![path],
        Action::Copy { from, to } | Action::Lookup { from, to, .. } => vec![from, to],
        Action::Store { .. } => Vec::new(),
    };
    let mut prefixes: Vec<String> = paths
        .into_iter()
        .filter_map(PathSpec::wildcard_prefix)
        .map(str::to_owned)
        .collect();
    let templates: Vec<&Template> = match action {
        Action::Set { value, .. } | Action::Store { value, .. } => vec![value],
        Action::Lookup {
            context, default, ..
        } => context.iter().chain(default.iter()).collect(),
        _ => Vec::new(),
    };
    for template in templates {
        prefixes.extend(template.wildcard_prefixes().map(str::to_owned));
    }
    prefixes
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::test_support::{astm_context, context, step};

    use super::*;

    fn run(operations: Value, raw: &[u8]) -> Result<String, StepError> {
        let environment = TransformEnvironment::in_memory().with_table(
            "tests",
            CodeTable::from_csv("from,to,display,context\nGLU,1520,Glucose,\nHGB,2010,Hemoglobin,\nGLU,1521,Glucose POCT,poct\n").unwrap(),
        );
        let transformer = MapTransformer::from_step(
            &step("map", json!({"operations": operations})),
            &environment,
        )
        .unwrap();
        let mut ctx = context(raw);
        transformer.apply(&mut ctx)?;
        Ok(String::from_utf8(ctx.document.to_bytes())
            .unwrap()
            .replace('\r', "\n"))
    }

    const MSG: &[u8] = b"MSH|^~\\&|LAB|HOSP|||20260929143005||ORU^R01|7|P|2.5\rPID|1||  0042 ||doe^jane||19800115\rOBX|1|NM|GLU||5.4\rOBX|2|NM|HGB||13\rOBX|3|NM|XYZ||1\r";

    #[test]
    fn edits_values() {
        let out = run(
            json!([
                {"set": {"path": "MSH-3", "value": "OXIM-{PID-5.1}"}},
                {"copy": {"from": "MSH-10", "to": "PID-2"}},
                {"trim": {"path": "PID-3"}},
                {"replace": {"path": "PID-3", "pattern": "^0+", "with": ""}},
                {"pad": {"path": "PID-3", "length": 6, "char": "0"}},
                {"upper": {"path": "PID-5.1"}},
                {"lower": {"path": "MSH-4"}},
                {"substring": {"path": "PID-5.2", "start": 1, "length": 2}},
                {"clear": {"path": "PID-1"}},
                {"date": {"path": "PID-7", "from": "hl7", "to": "%d.%m.%Y"}},
            ]),
            MSG,
        )
        .unwrap();
        assert!(out.starts_with("MSH|^~\\&|OXIM-doe|hosp|"), "{out}");
        assert!(
            out.contains("\nPID||7|000042||DOE^an||15.01.1980\n"),
            "{out}"
        );
    }

    #[test]
    fn maps_every_occurrence_with_lookups() {
        let out = run(
            json!([
                {"lookup": {"table": "tests", "from": "OBX[*]-3.1", "to": "OBX[*]-3.2", "column": "display"}},
                {"lookup": {"table": "tests", "from": "OBX[*]-3.1", "on_missing": "keep"}},
                {"when": {"path": "OBX[*]-3.1", "equals": "2010"}, "set": {"path": "OBX[*]-6", "value": "g/dL"}},
            ]),
            MSG,
        )
        .unwrap();
        assert!(out.contains("OBX|1|NM|1520^Glucose||5.4\n"), "{out}");
        assert!(out.contains("OBX|2|NM|2010^Hemoglobin||13|g/dL\n"), "{out}");
        assert!(out.contains("OBX|3|NM|XYZ^XYZ||1\n"), "{out}");
    }

    #[test]
    fn lookup_handles_contexts_defaults_and_errors() {
        let out = run(
            json!([
                {"lookup": {"table": "tests", "from": "OBX-3.1", "context": "poct"}},
                {"lookup": {"table": "tests", "from": "OBX[3]-3.1", "default": "UNMAPPED-{OBX[3]-3.1}"}},
            ]),
            MSG,
        )
        .unwrap();
        assert!(out.contains("OBX|1|NM|1521||"), "{out}");
        assert!(out.contains("OBX|3|NM|UNMAPPED-XYZ||"), "{out}");
        let empty = run(
            json!([{"lookup": {"table": "tests", "from": "OBX[3]-3.1", "on_missing": "empty"}}]),
            MSG,
        )
        .unwrap();
        assert!(empty.contains("OBX|3|NM|||1"), "{empty}");
        let error = run(
            json!([{"lookup": {"table": "tests", "from": "OBX[3]-3.1", "on_missing": "error"}}]),
            MSG,
        )
        .unwrap_err();
        assert!(error.message.contains("XYZ"), "{error}");
    }

    #[test]
    fn stores_variables_and_applies_conditions() {
        let out = run(
            json!([
                {"store": {"path": "PID-5.1", "as": "family"}},
                {"store": {"value": "{$family}-{MSH-10}", "as": "key"}},
                {"when": {"variable": "key", "equals": "doe-7"}, "set": {"path": "PID-19", "value": "{$key}"}},
                {"when": {"path": "MSH-9.1", "equals": "ADT"}, "set": {"path": "PID-20", "value": "no"}},
            ]),
            MSG,
        )
        .unwrap();
        assert!(out.contains("|doe-7\n"), "{out}");
        assert!(!out.contains("|no"), "{out}");
    }

    #[test]
    fn date_follows_on_invalid() {
        let bad = b"MSH|^~\\&\rPID|1||42||x||not-a-date\r";
        assert!(
            run(
                json!([{"date": {"path": "PID-7", "from": "hl7", "to": "iso"}}]),
                bad
            )
            .is_err()
        );
        let kept = run(
            json!([{"date": {"path": "PID-7", "from": "hl7", "to": "iso", "on_invalid": "keep"}}]),
            bad,
        )
        .unwrap();
        assert!(kept.contains("not-a-date"));
        let emptied = run(
            json!([{"date": {"path": "PID-7", "from": "hl7", "to": "iso", "on_invalid": "empty"}}]),
            bad,
        )
        .unwrap();
        assert!(emptied.ends_with("PID|1||42||x||\n"), "{emptied}");
    }

    #[test]
    fn works_on_astm_records() {
        let environment = TransformEnvironment::in_memory()
            .with_table("tests", CodeTable::from_csv("from,to\nGLU,1520\n").unwrap());
        let transformer = MapTransformer::from_step(
            &step(
                "map",
                json!({"operations": [{"lookup": {"table": "tests", "from": "R[*]-3.4"}}]}),
            ),
            &environment,
        )
        .unwrap();
        let mut ctx = astm_context(b"H|\\^&\rP|1\rO|1|S1\rR|1|^^^GLU|5.4\rR|2|^^^NA|140\rL|1\r");
        transformer.apply(&mut ctx).unwrap();
        let out = String::from_utf8(ctx.document.to_bytes()).unwrap();
        assert!(out.contains("R|1|^^^1520|5.4\r"), "{out}");
        assert!(out.contains("R|2|^^^NA|140\r"), "{out}");
    }

    #[test]
    fn rejects_bad_operations() {
        let environment = TransformEnvironment::in_memory();
        for bad in [
            json!({}),
            json!({"operations": "set"}),
            json!({"operations": [{}]}),
            json!({"operations": [{"set": {"path": "A-1", "value": "x"}, "copy": {"from": "a", "to": "b"}}]}),
            json!({"operations": [{"set": {"value": "x"}}]}),
            json!({"operations": [{"set": {"path": "PID-3", "valeu": "x"}}]}),
            json!({"operations": [{"copy": {"from": "OBX[*]-3", "to": "NTE[*]-3"}}]}),
            json!({"operations": [{"pad": {"path": "PID-3", "length": 5, "char": "ab"}}]}),
            json!({"operations": [{"replace": {"path": "PID-3", "pattern": "("}}]}),
            json!({"operations": [{"date": {"path": "PID-7", "from": "hl7", "to": "yyyy"}}]}),
            json!({"operations": [{"lookup": {"table": "missing", "from": "OBX-3"}}]}),
            json!({"operations": [{"store": {"path": "OBX[*]-3", "as": "x"}}]}),
            json!({"operations": [{"store": {"path": "PID-3"}}]}),
            json!({"operations": [{"when": {"path": "A-1"}, "set": {"path": "A-1", "value": ""}}]}),
        ] {
            assert!(
                MapTransformer::from_step(&step("map", bad.clone()), &environment).is_err(),
                "{bad}"
            );
        }
    }
}
