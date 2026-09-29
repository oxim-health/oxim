//! Property tests: panic freedom, losslessness and edit consistency.

// Strategy helpers build fixed, known-valid inputs.
#![allow(clippy::unwrap_used)]

use oxim_formats::{
    Alignment, DelimitedDocument, DelimitedOptions, FixedField, FixedWidthDocument,
    FixedWidthLayout, JsonDocument, JsonOptions, JsonValue, XmlDocument, XmlOptions,
};
use proptest::prelude::*;

fn interesting_bytes(alphabet: &'static [u8]) -> impl Strategy<Value = Vec<u8>> {
    let byte = prop_oneof![4 => prop::sample::select(alphabet), 1 => any::<u8>()];
    prop::collection::vec(byte, 0..300)
}

// ---------- JSON ----------

fn json_value() -> impl Strategy<Value = JsonValue> {
    let leaf = prop_oneof![
        Just(JsonValue::Null),
        any::<bool>().prop_map(JsonValue::Bool),
        "-?(0|[1-9][0-9]{0,4})(\\.[0-9]{1,4})?".prop_map(|n| serde_json::from_str(&n).unwrap()),
        any::<String>().prop_map(JsonValue::String),
    ];
    leaf.prop_recursive(4, 32, 6, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..6).prop_map(JsonValue::Array),
            prop::collection::vec(("[a-z]{1,6}", inner), 0..6)
                .prop_map(|entries| JsonValue::Object(entries.into_iter().collect())),
        ]
    })
}

fn json_path() -> impl Strategy<Value = String> {
    prop::collection::vec(("[a-z]{1,4}", prop::option::of(0usize..4)), 1..4).prop_map(|steps| {
        steps
            .into_iter()
            .map(|(name, index)| match index {
                Some(index) => format!("{name}[{index}]"),
                None => name,
            })
            .collect::<Vec<_>>()
            .join(".")
    })
}

// ---------- XML ----------

fn xml_text() -> BoxedStrategy<String> {
    prop::collection::vec(
        prop::sample::select(vec![
            "x", " ", "\n", "&amp;", "&lt;", "&#65;", "&#x42;", "é", "ş", ">",
        ]),
        0..5,
    )
    .prop_map(|parts| parts.concat())
    .boxed()
}

fn xml_element() -> impl Strategy<Value = String> {
    let name = prop::sample::select(vec!["a", "b", "ns:c", "test", "x1"]);
    let attributes = prop::collection::btree_map(
        prop::sample::select(vec!["id", "code", "ns:type"]),
        (
            xml_text(),
            prop::bool::ANY,
            prop::sample::select(vec![" ", "\n  "]),
        ),
        0..3,
    );
    let leaf = (name.clone(), attributes.clone(), prop::bool::ANY).prop_map(
        |(name, attributes, spaced)| {
            format!(
                "<{name}{}{}/>",
                render_attributes(&attributes),
                if spaced { " " } else { "" }
            )
        },
    );
    leaf.prop_recursive(4, 24, 5, move |inner| {
        let child = prop_oneof![
            inner,
            xml_text(),
            "[a-z <>&]{0,8}".prop_map(|t| format!("<![CDATA[{t}]]>")),
            "[a-z ]{0,8}".prop_map(|t| format!("<!--{t}-->")),
        ];
        (
            name.clone(),
            attributes.clone(),
            prop::collection::vec(child, 0..5),
        )
            .prop_map(|(name, attributes, children)| {
                format!(
                    "<{name}{}>{}</{name}>",
                    render_attributes(&attributes),
                    children.concat()
                )
            })
    })
}

fn render_attributes(
    attributes: &std::collections::BTreeMap<&str, (String, bool, &str)>,
) -> String {
    attributes
        .iter()
        .map(|(name, (value, single, space))| {
            let quote = if *single { '\'' } else { '"' };
            format!("{space}{name}={quote}{value}{quote}")
        })
        .collect()
}

// ---------- tables ----------

fn csv_options() -> impl Strategy<Value = DelimitedOptions> {
    prop_oneof![
        Just(DelimitedOptions::csv()),
        Just(DelimitedOptions::tsv()),
        Just(DelimitedOptions::csv().with_escape(Some(b'\\'))),
        Just(
            DelimitedOptions::csv()
                .with_delimiter(b';')
                .with_quote(Some(b'\''))
        ),
    ]
}

fn fixed_layout() -> FixedWidthLayout {
    FixedWidthLayout::new(vec![
        FixedField::new("left", 0, 8),
        FixedField::new("right", 8, 6)
            .aligned(Alignment::Right)
            .padded_with(b'0'),
        FixedField::new("tail", 15, 5),
    ])
    .unwrap()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    #[test]
    fn json_never_panics(input in interesting_bytes(b"{}[],:\"\\0123456789.eE-truefalsn ")) {
        if let Ok(doc) = JsonDocument::parse(&input, &JsonOptions::default()) {
            prop_assert_eq!(doc.to_bytes(), input);
        }
    }

    #[test]
    fn json_round_trips_and_edits(value in json_value(), pretty in any::<bool>(), path in json_path(), text in any::<String>()) {
        let bytes = if pretty { serde_json::to_vec_pretty(&value) } else { serde_json::to_vec(&value) }.unwrap();
        let mut doc = JsonDocument::parse(&bytes, &JsonOptions::default()).unwrap();
        prop_assert_eq!(doc.to_bytes(), bytes.clone());
        prop_assert_eq!(doc.value(), &value);
        if doc.set_text(&path, &text).is_ok() {
            prop_assert_eq!(doc.get_text(&path).unwrap(), Some(text.clone()));
            let reparsed = JsonDocument::parse(&doc.to_bytes(), &JsonOptions::default()).unwrap();
            prop_assert_eq!(reparsed.get_text(&path).unwrap(), Some(text));
        }
    }

    #[test]
    fn xml_never_panics(input in interesting_bytes(b"<>/?!-[]=\"'&;#xamplt a:b\n")) {
        if let Ok(doc) = XmlDocument::parse(&input, &XmlOptions::default()) {
            prop_assert_eq!(doc.to_bytes(), input);
        }
    }

    #[test]
    fn xml_round_trips_and_edits(
        element in xml_element(),
        prolog in prop::sample::select(vec!["", "<?xml version=\"1.0\"?>\n", "\u{feff}<!-- c -->\n"]),
        text in "[a-zA-Z0-9 <>&\"'şé\t\n]{0,12}",
        child in 1usize..4,
    ) {
        let input = format!("{prolog}{element}\n");
        let mut doc = XmlDocument::parse(input.as_bytes(), &XmlOptions::default()).unwrap();
        prop_assert_eq!(doc.to_bytes(), input.as_bytes());

        let root = doc.root_name();
        let count = doc.count(&format!("/{root}/item")).unwrap();
        let path = format!("/{root}/item[{}]", child.min(count + 1));
        doc.set(&path, &text).unwrap();
        doc.set(&format!("{path}/@note"), &text).unwrap();
        let reparsed = XmlDocument::parse(&doc.to_bytes(), &XmlOptions::default()).unwrap();
        prop_assert_eq!(reparsed.get(&path).unwrap(), Some(text.clone()));
        prop_assert_eq!(reparsed.get(&format!("{path}/@note")).unwrap(), Some(text));
    }

    #[test]
    fn delimited_accepts_and_round_trips_any_input(
        input in interesting_bytes(b",;\t\"'\\\r\nab "),
        options in csv_options(),
    ) {
        let doc = DelimitedDocument::parse(&input, &options).unwrap();
        prop_assert_eq!(doc.to_bytes(), input);
        for row in 0..doc.row_count() {
            for column in 0..doc.field_count(row).unwrap_or(0) {
                prop_assert!(doc.get(row, column).is_some());
            }
        }
    }

    #[test]
    fn delimited_edits_survive_reparsing(
        table in prop::collection::vec(prop::collection::vec("[a-z,;\t\"'\\\\\r\n ]{0,6}", 1..4), 0..5),
        options in csv_options(),
        row in 0usize..6,
        column in 0usize..5,
        value in any::<String>(),
    ) {
        let mut doc = DelimitedDocument::parse(b"", &options).unwrap();
        for values in &table {
            doc.push_row(values).unwrap();
        }
        let row = row.min(doc.row_count());
        doc.set(row, column, &value).unwrap();
        prop_assert_eq!(doc.get(row, column), Some(value.clone()));
        let reparsed = DelimitedDocument::parse(&doc.to_bytes(), &options).unwrap();
        prop_assert_eq!(reparsed.get(row, column), Some(value));
        for (r, values) in table.iter().enumerate() {
            for (c, expected) in values.iter().enumerate() {
                if (r, c) != (row, column) {
                    let actual = reparsed.get(r, c);
                    prop_assert_eq!(actual.as_deref(), Some(expected.as_str()));
                }
            }
        }
    }

    #[test]
    fn fixed_width_round_trips_and_edits(
        input in interesting_bytes(b"ab01 \r\n"),
        record in 0usize..4,
        left in "[a-zA-Z0-9]([a-zA-Z0-9 ]{0,6}[a-zA-Z0-9])?",
        right in "[1-9][0-9]{0,5}",
    ) {
        let mut doc = FixedWidthDocument::parse(&input, &fixed_layout()).unwrap();
        prop_assert_eq!(doc.to_bytes(), input);
        let record = record.min(doc.record_count());
        doc.set(record, "left", &left).unwrap();
        doc.set(record, "right", &right).unwrap();
        prop_assert_eq!(doc.get(record, "left").unwrap(), Some(left.clone()));
        let reparsed = FixedWidthDocument::parse(&doc.to_bytes(), &fixed_layout()).unwrap();
        prop_assert_eq!(reparsed.get(record, "left").unwrap(), Some(left));
        prop_assert_eq!(reparsed.get(record, "right").unwrap(), Some(right));
    }
}
