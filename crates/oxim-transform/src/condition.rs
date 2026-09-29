//! Conditions: the `condition` filter and the `when:` of map operations.

use std::cmp::Ordering;

use oxim_core::{EngineError, MessageContext, StepError};
use oxim_model::Decimal;
use regex::{Regex, RegexBuilder};
use serde_json::Value;

use crate::path::{PathSpec, occurrences};
use crate::settings::{Obj, config_error};

/// Where a leaf condition reads its value.
#[derive(Debug, Clone)]
enum Source {
    Path(PathSpec),
    Variable(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Comparison {
    Greater,
    GreaterOrEqual,
    Less,
    LessOrEqual,
}

#[derive(Debug, Clone)]
enum Test {
    Equals(String),
    NotEquals(String),
    In(Vec<String>),
    NotIn(Vec<String>),
    Matches(Regex),
    Exists(bool),
    Empty(bool),
    Compare(Comparison, String),
}

#[derive(Debug, Clone)]
struct Leaf {
    source: Source,
    test: Test,
    case_insensitive: bool,
}

/// A condition tree.
///
/// ```yaml
/// all:
///   - {path: MSH-9.1, in: [ORU, OUL]}
///   - any:
///       - {path: OBX[*]-8, equals: H}
///       - {path: OBX[*]-8, equals: L}
///   - not: {variable: skip, equals: "yes"}
///   - {path: OBX-5, greater_than: "10.5"}
///   - {path: PID-5.1, matches: "^(?i)test", case_insensitive: true}
/// ```
///
/// A leaf reads one value, from a document `path` or a `variable`, and
/// applies exactly one test:
///
/// | Test | Holds when |
/// |---|---|
/// | `equals: X` / `not_equals: X` | the value is (is not) `X`; a missing value is not equal |
/// | `in: [..]` / `not_in: [..]` | the value is (is not) one of the list |
/// | `matches: REGEX` | the value matches the regular expression (Rust `regex` syntax) |
/// | `exists: true` | the value is present and not empty (`false` inverts) |
/// | `empty: true` | the value is missing or empty (`false` inverts) |
/// | `greater_than`, `greater_or_equal`, `less_than`, `less_or_equal` | both sides are decimal numbers and compare as stated |
///
/// `case_insensitive: true` makes `equals`, `not_equals`, `in`, `not_in`
/// and `matches` ignore letter case.
///
/// Numeric comparisons are exact decimal comparisons of the text (`5.40`
/// equals `5.4`, `1e2` equals `100`); values that are not plain decimal
/// numbers, such as `<0.5` or `POS`, never satisfy a numeric test. Quote
/// numbers in YAML (`"5.40"`) when their exact text matters.
///
/// A path with a `[*]` wildcard holds when any occurrence satisfies the
/// test, except inside a map operation that iterates the same wildcard,
/// where it refers to the current occurrence.
#[derive(Debug, Clone)]
pub struct Condition(Node);

#[derive(Debug, Clone)]
enum Node {
    All(Vec<Node>),
    Any(Vec<Node>),
    Not(Box<Node>),
    Leaf(Box<Leaf>),
}

const MAX_DEPTH: usize = 32;

impl Condition {
    /// Parses a condition from its configuration value.
    pub fn parse(value: &Value) -> Result<Self, EngineError> {
        Node::parse("condition", value, 0).map(Self)
    }

    pub(crate) fn parse_at(at: &str, value: &Value) -> Result<Self, EngineError> {
        Node::parse(at, value, 0).map(Self)
    }

    /// Evaluates the condition. `occurrence` binds `[*]` wildcards to one
    /// occurrence; without it a wildcard holds when any occurrence does.
    pub fn evaluate(
        &self,
        context: &MessageContext,
        occurrence: Option<usize>,
    ) -> Result<bool, StepError> {
        self.0.evaluate(context, occurrence)
    }

    /// The wildcard prefixes of the paths the condition reads.
    pub(crate) fn wildcard_prefixes(&self) -> Vec<String> {
        let mut prefixes = Vec::new();
        self.0.collect_prefixes(&mut prefixes);
        prefixes
    }
}

impl Node {
    fn parse(at: &str, value: &Value, depth: usize) -> Result<Self, EngineError> {
        if depth > MAX_DEPTH {
            return Err(config_error(at, "conditions are nested too deeply"));
        }
        let obj = Obj::new(at, value)?;
        let group = |key: &str| -> Result<Option<Vec<Node>>, EngineError> {
            let Some(items) = obj.value(key) else {
                return Ok(None);
            };
            let Value::Array(items) = items else {
                return Err(config_error(
                    at,
                    format!("{key:?} must be a list of conditions"),
                ));
            };
            if items.is_empty() {
                return Err(config_error(at, format!("{key:?} must not be empty")));
            }
            items
                .iter()
                .enumerate()
                .map(|(i, item)| Node::parse(&format!("{at}.{key}[{i}]"), item, depth + 1))
                .collect::<Result<Vec<_>, _>>()
                .map(Some)
        };
        let is_group = ["all", "any", "not"].iter().any(|key| obj.has(key));
        if is_group {
            if obj.map.len() != 1 {
                return Err(config_error(
                    at,
                    "a condition group has exactly one of all, any or not and nothing else",
                ));
            }
            if let Some(nodes) = group("all")? {
                return Ok(Self::All(nodes));
            }
            if let Some(nodes) = group("any")? {
                return Ok(Self::Any(nodes));
            }
            if let Some(inner) = obj.value("not") {
                return Ok(Self::Not(Box::new(Node::parse(
                    &format!("{at}.not"),
                    inner,
                    depth + 1,
                )?)));
            }
        }
        Leaf::parse(at, obj).map(|leaf| Self::Leaf(Box::new(leaf)))
    }

    fn evaluate(
        &self,
        context: &MessageContext,
        occurrence: Option<usize>,
    ) -> Result<bool, StepError> {
        match self {
            Self::All(nodes) => {
                for node in nodes {
                    if !node.evaluate(context, occurrence)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            Self::Any(nodes) => {
                for node in nodes {
                    if node.evaluate(context, occurrence)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            Self::Not(node) => Ok(!node.evaluate(context, occurrence)?),
            Self::Leaf(leaf) => leaf.evaluate(context, occurrence),
        }
    }

    fn collect_prefixes(&self, out: &mut Vec<String>) {
        match self {
            Self::All(nodes) | Self::Any(nodes) => {
                for node in nodes {
                    node.collect_prefixes(out);
                }
            }
            Self::Not(node) => node.collect_prefixes(out),
            Self::Leaf(leaf) => {
                if let Source::Path(path) = &leaf.source
                    && let Some(prefix) = path.wildcard_prefix()
                {
                    out.push(prefix.to_owned());
                }
            }
        }
    }
}

const LEAF_KEYS: &[&str] = &[
    "path",
    "variable",
    "equals",
    "not_equals",
    "in",
    "not_in",
    "matches",
    "exists",
    "empty",
    "greater_than",
    "greater_or_equal",
    "less_than",
    "less_or_equal",
    "case_insensitive",
];

impl Leaf {
    fn parse(at: &str, obj: Obj<'_>) -> Result<Self, EngineError> {
        obj.only(LEAF_KEYS)?;
        let source = match (obj.text("path")?, obj.text("variable")?) {
            (Some(path), None) => Source::Path(PathSpec::parse(at, &path)?),
            (None, Some(variable)) => Source::Variable(variable),
            _ => {
                return Err(config_error(
                    at,
                    "a condition needs exactly one of path or variable",
                ));
            }
        };
        let case_insensitive = obj.bool("case_insensitive")?.unwrap_or(false);
        let tests: Vec<&str> = LEAF_KEYS[2..13]
            .iter()
            .copied()
            .filter(|key| obj.has(key))
            .collect();
        let [test] = tests.as_slice() else {
            return Err(config_error(
                at,
                "a condition needs exactly one test: equals, not_equals, in, not_in, matches, exists, empty, greater_than, greater_or_equal, less_than or less_or_equal",
            ));
        };
        let fold = |text: String| {
            if case_insensitive {
                text.to_lowercase()
            } else {
                text
            }
        };
        let number = |key: &str| -> Result<String, EngineError> {
            let text = obj.required_text(key)?;
            Decimal::new(text.trim())
                .map(|_| text.trim().to_owned())
                .map_err(|_| {
                    config_error(
                        at,
                        format!("{key:?} must be a decimal number, not {text:?}"),
                    )
                })
        };
        let test = match *test {
            "equals" => Test::Equals(fold(obj.required_text("equals")?)),
            "not_equals" => Test::NotEquals(fold(obj.required_text("not_equals")?)),
            "in" => Test::In(obj.text_list("in")?.into_iter().map(fold).collect()),
            "not_in" => Test::NotIn(obj.text_list("not_in")?.into_iter().map(fold).collect()),
            "matches" => {
                let pattern = obj.required_text("matches")?;
                Test::Matches(
                    RegexBuilder::new(&pattern)
                        .case_insensitive(case_insensitive)
                        .size_limit(1 << 20)
                        .build()
                        .map_err(|e| {
                            config_error(at, format!("invalid regular expression: {e}"))
                        })?,
                )
            }
            "exists" => Test::Exists(bool_test(obj, "exists")?),
            "empty" => Test::Empty(bool_test(obj, "empty")?),
            "greater_than" => Test::Compare(Comparison::Greater, number("greater_than")?),
            "greater_or_equal" => {
                Test::Compare(Comparison::GreaterOrEqual, number("greater_or_equal")?)
            }
            "less_than" => Test::Compare(Comparison::Less, number("less_than")?),
            _ => Test::Compare(Comparison::LessOrEqual, number("less_or_equal")?),
        };
        Ok(Self {
            source,
            test,
            case_insensitive,
        })
    }

    fn evaluate(
        &self,
        context: &MessageContext,
        occurrence: Option<usize>,
    ) -> Result<bool, StepError> {
        match &self.source {
            Source::Variable(name) => {
                Ok(self.test(context.variables.get(name).map(String::as_str)))
            }
            Source::Path(path) => match (path.wildcard_prefix(), occurrence) {
                (Some(prefix), None) => {
                    let count = occurrences(&context.document, prefix)?;
                    for n in 1..=count {
                        if self.test(context.document.get(&path.resolve(Some(n)))?.as_deref()) {
                            return Ok(true);
                        }
                    }
                    Ok(false)
                }
                _ => Ok(self.test(context.document.get(&path.resolve(occurrence))?.as_deref())),
            },
        }
    }

    fn test(&self, value: Option<&str>) -> bool {
        let folded = |value: &str| {
            if self.case_insensitive {
                value.to_lowercase()
            } else {
                value.to_owned()
            }
        };
        match &self.test {
            Test::Equals(expected) => value.is_some_and(|v| folded(v) == *expected),
            Test::NotEquals(expected) => !value.is_some_and(|v| folded(v) == *expected),
            Test::In(list) => value.is_some_and(|v| list.contains(&folded(v))),
            Test::NotIn(list) => !value.is_some_and(|v| list.contains(&folded(v))),
            Test::Matches(regex) => value.is_some_and(|v| regex.is_match(v)),
            Test::Exists(wanted) => value.is_some_and(|v| !v.is_empty()) == *wanted,
            Test::Empty(wanted) => value.is_none_or(str::is_empty) == *wanted,
            Test::Compare(comparison, bound) => value
                .and_then(|v| compare_decimals(v.trim(), bound))
                .is_some_and(|ordering| match comparison {
                    Comparison::Greater => ordering == Ordering::Greater,
                    Comparison::GreaterOrEqual => ordering != Ordering::Less,
                    Comparison::Less => ordering == Ordering::Less,
                    Comparison::LessOrEqual => ordering != Ordering::Greater,
                }),
        }
    }
}

fn bool_test(obj: Obj<'_>, key: &str) -> Result<bool, EngineError> {
    obj.bool(key)?
        .ok_or_else(|| config_error(obj.at, format!("{key:?} must be true or false")))
}

/// A decimal number as sign, significant digits and exponent:
/// `value = ±0.d1d2d3... × 10^exponent`.
#[derive(Debug, PartialEq, Eq)]
struct Number {
    negative: bool,
    digits: Vec<u8>,
    exponent: i64,
}

fn parse_number(text: &str) -> Option<Number> {
    Decimal::new(text).ok()?;
    let (negative, rest) = match text.as_bytes().first()? {
        b'-' => (true, &text[1..]),
        b'+' => (false, &text[1..]),
        _ => (false, text),
    };
    let (mantissa, exponent) = match rest.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => (mantissa, exponent.parse::<i64>().ok()?),
        None => (rest, 0),
    };
    let (integer, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let mut digits: Vec<u8> = integer
        .bytes()
        .chain(fraction.bytes())
        .map(|b| b - b'0')
        .collect();
    let mut point = i64::try_from(integer.len()).ok()? + exponent;
    let leading = digits.iter().take_while(|&&d| d == 0).count();
    digits.drain(..leading);
    point -= i64::try_from(leading).ok()?;
    while digits.last() == Some(&0) {
        digits.pop();
    }
    if digits.is_empty() {
        return Some(Number {
            negative: false,
            digits,
            exponent: 0,
        });
    }
    Some(Number {
        negative,
        digits,
        exponent: point,
    })
}

/// Compares two decimal numbers exactly, or returns `None` when either is
/// not a decimal number.
pub(crate) fn compare_decimals(a: &str, b: &str) -> Option<Ordering> {
    let (a, b) = (parse_number(a)?, parse_number(b)?);
    let magnitude = |x: &Number, y: &Number| -> Ordering {
        match (x.digits.is_empty(), y.digits.is_empty()) {
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            (false, false) => x
                .exponent
                .cmp(&y.exponent)
                .then_with(|| x.digits.cmp(&y.digits)),
        }
    };
    Some(match (a.negative, b.negative) {
        (false, false) => magnitude(&a, &b),
        (true, true) => magnitude(&b, &a),
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::test_support::context;

    use super::*;

    fn eval(condition: Value, raw: &[u8]) -> bool {
        Condition::parse(&condition)
            .unwrap()
            .evaluate(&context(raw), None)
            .unwrap()
    }

    const ORU: &[u8] = b"MSH|^~\\&|LAB|HOSP|||||ORU^R01|1|P|2.5\rPID|1||42||Doe^Jane\rOBX|1|NM|GLU||5.40|||N\rOBX|2|NM|HGB||11.1|||L\r";

    #[test]
    fn evaluates_leaf_tests() {
        assert!(eval(json!({"path": "MSH-9.1", "equals": "ORU"}), ORU));
        assert!(!eval(json!({"path": "MSH-9.1", "equals": "oru"}), ORU));
        assert!(eval(
            json!({"path": "MSH-9.1", "equals": "oru", "case_insensitive": true}),
            ORU
        ));
        assert!(eval(json!({"path": "MSH-9.1", "not_equals": "ADT"}), ORU));
        assert!(eval(json!({"path": "PID-99", "not_equals": "X"}), ORU));
        assert!(eval(json!({"path": "MSH-9.1", "in": ["ORU", "OUL"]}), ORU));
        assert!(eval(json!({"path": "MSH-9.1", "not_in": ["ADT"]}), ORU));
        assert!(eval(json!({"path": "PID-5.1", "matches": "^D.e$"}), ORU));
        assert!(eval(json!({"path": "PID-5.1", "exists": true}), ORU));
        assert!(eval(json!({"path": "PID-7", "exists": false}), ORU));
        assert!(eval(json!({"path": "PID-7", "empty": true}), ORU));
        assert!(eval(json!({"path": "OBX-5", "greater_than": "5.39"}), ORU));
        assert!(eval(
            json!({"path": "OBX-5", "greater_or_equal": "5.4"}),
            ORU
        ));
        assert!(!eval(json!({"path": "OBX-5", "less_than": "5.4"}), ORU));
        assert!(eval(json!({"path": "OBX-5", "less_or_equal": 5.4}), ORU));
        assert!(!eval(json!({"path": "PID-5.1", "greater_than": "0"}), ORU));
    }

    #[test]
    fn evaluates_groups_and_wildcards() {
        assert!(eval(
            json!({"all": [
                {"path": "MSH-9.1", "equals": "ORU"},
                {"any": [{"path": "OBX[*]-8", "equals": "H"}, {"path": "OBX[*]-8", "equals": "L"}]},
                {"not": {"path": "PID-3", "equals": "0"}}
            ]}),
            ORU
        ));
        let condition = Condition::parse(&json!({"path": "OBX[*]-8", "equals": "L"})).unwrap();
        let ctx = context(ORU);
        assert!(condition.evaluate(&ctx, None).unwrap());
        assert!(!condition.evaluate(&ctx, Some(1)).unwrap());
        assert!(condition.evaluate(&ctx, Some(2)).unwrap());
        assert_eq!(condition.wildcard_prefixes(), ["OBX"]);
    }

    #[test]
    fn reads_variables() {
        let condition = Condition::parse(&json!({"variable": "route", "equals": "lab"})).unwrap();
        let mut ctx = context(ORU);
        assert!(!condition.evaluate(&ctx, None).unwrap());
        ctx.variables.insert("route".into(), "lab".into());
        assert!(condition.evaluate(&ctx, None).unwrap());
    }

    #[test]
    fn rejects_malformed_conditions() {
        for bad in [
            json!("text"),
            json!({"path": "PID-3"}),
            json!({"path": "PID-3", "equals": "a", "in": ["b"]}),
            json!({"equals": "a"}),
            json!({"path": "PID-3", "variable": "x", "equals": "a"}),
            json!({"path": "PID-3", "equal": "a"}),
            json!({"all": []}),
            json!({"all": [{"path": "PID-3", "exists": true}], "any": []}),
            json!({"path": "PID-3", "matches": "("}),
            json!({"path": "PID-3", "greater_than": "ten"}),
            json!({"path": "PID-3", "exists": "yes"}),
            json!({"path": "PID-3", "in": "a"}),
        ] {
            assert!(Condition::parse(&bad).is_err(), "{bad}");
        }
        let mut deep = json!({"path": "PID-3", "exists": true});
        for _ in 0..40 {
            deep = json!({"not": deep});
        }
        assert!(Condition::parse(&deep).is_err());
    }

    #[test]
    fn compares_decimals_exactly() {
        for (a, b, expected) in [
            ("5.40", "5.4", Ordering::Equal),
            ("1e2", "100", Ordering::Equal),
            ("0.1", "0.10000000000000000001", Ordering::Less),
            ("-2", "-10", Ordering::Greater),
            ("-0", "0", Ordering::Equal),
            ("007", "7.0", Ordering::Equal),
            ("12.5E-1", "1.25", Ordering::Equal),
            ("9999999999999999999999", "1e22", Ordering::Less),
            ("0.00", "-0.001", Ordering::Greater),
        ] {
            assert_eq!(compare_decimals(a, b), Some(expected), "{a} vs {b}");
        }
        assert_eq!(compare_decimals("<0.5", "1"), None);
        assert_eq!(compare_decimals("POS", "1"), None);
    }
}
