//! Property tests: round trips, chunked splitting and panic freedom.

use oxim_poct1a::{Element, Message, Node, SplitEvent, Splitter, WriteOptions};
use proptest::prelude::*;

fn xml_text() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop_oneof![
            4 => prop::sample::select(vec!['a', 'Z', '0', ' ', '<', '>', '&', '"', '\'', ']', '\t', '\n', '\r', 'ş', 'İ', '€']),
            1 => any::<char>(),
        ]
        .prop_filter("XML 1.0 character", |c| {
            matches!(*c, '\t' | '\n' | '\r' | '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..)
        }),
        0..12,
    )
    .prop_map(String::from_iter)
}

fn element_name() -> impl Strategy<Value = &'static str> {
    prop::sample::select(vec![
        "HDR",
        "HDR.control_id",
        "OBS",
        "OBS.value",
        "SVC",
        "PT",
        "x",
        "_y",
        "a-b.c",
        "Ölçüm",
    ])
}

fn attributes() -> impl Strategy<Value = Vec<(&'static str, String)>> {
    prop::sample::subsequence(vec!["V", "U", "SN", "a"], 0..=4)
        .prop_flat_map(|names| {
            let count = names.len();
            (Just(names), prop::collection::vec(xml_text(), count))
        })
        .prop_map(|(names, values)| names.into_iter().zip(values).collect())
}

fn tree() -> impl Strategy<Value = Element> {
    let leaf = (element_name(), attributes()).prop_map(|(name, attributes)| {
        attributes
            .into_iter()
            .fold(Element::new(name), |element, (key, value)| {
                element.attribute(key, value)
            })
    });
    leaf.prop_recursive(4, 40, 4, |inner| {
        (
            element_name(),
            attributes(),
            prop::collection::vec(
                prop_oneof![
                    inner.prop_map(Node::Element),
                    xml_text().prop_map(Node::Text)
                ],
                0..4,
            ),
        )
            .prop_map(|(name, attributes, children)| {
                let mut element = attributes
                    .into_iter()
                    .fold(Element::new(name), |element, (key, value)| {
                        element.attribute(key, value)
                    });
                for child in children {
                    match child {
                        Node::Element(child) => element.push_child(child),
                        Node::Text(text) => element.push_text(&text),
                    }
                }
                element
            })
    })
}

fn xmlish_bytes() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(
        prop_oneof![
            4 => prop::sample::select(b"<>/?!-[]'\"= aAxml\r\nCDAT&;#".to_vec()),
            1 => any::<u8>(),
        ],
        0..400,
    )
}

fn split_in_chunks(stream: &[u8], cuts: &[usize]) -> Vec<SplitEvent> {
    let mut splitter = Splitter::default();
    let mut events = Vec::new();
    let mut cuts: Vec<usize> = cuts.iter().map(|c| c % (stream.len() + 1)).collect();
    cuts.sort_unstable();
    let mut start = 0;
    for cut in cuts.into_iter().chain([stream.len()]) {
        splitter.push(&stream[start..cut]);
        start = cut;
        events.extend(std::iter::from_fn(|| splitter.next_event()));
    }
    events.extend(splitter.finish());
    events
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    #[test]
    fn built_messages_parse_back_identically(root in tree(), declaration in any::<bool>()) {
        let message = Message::from_element_with(root.clone(), &WriteOptions::with_declaration(declaration)).unwrap();
        let parsed = Message::parse(message.as_bytes()).unwrap();
        prop_assert_eq!(parsed.root(), &root);
        prop_assert_eq!(parsed.as_bytes(), message.as_bytes());
    }

    #[test]
    fn documents_survive_any_chunking(
        roots in prop::collection::vec((tree(), any::<bool>()), 1..5),
        separators in prop::collection::vec(prop::sample::select(vec!["", " ", "\r\n", "\n\n\t"]), 5),
        cuts in prop::collection::vec(any::<usize>(), 0..20),
    ) {
        let mut stream = Vec::new();
        let mut expected = Vec::new();
        for (i, (root, declaration)) in roots.into_iter().enumerate() {
            let message = Message::from_element_with(root, &WriteOptions::with_declaration(declaration)).unwrap();
            stream.extend_from_slice(separators[i].as_bytes());
            stream.extend_from_slice(message.as_bytes());
            expected.push(SplitEvent::Document(message.into_bytes()));
        }
        let events = split_in_chunks(&stream, &cuts);
        prop_assert_eq!(&events, &expected);
        for event in events {
            if let SplitEvent::Document(bytes) = event {
                prop_assert!(Message::parse(&bytes).is_ok());
            }
        }
    }

    #[test]
    fn arbitrary_bytes_never_panic_and_are_accounted_for(
        stream in xmlish_bytes(),
        cuts in prop::collection::vec(any::<usize>(), 0..20),
    ) {
        let events = split_in_chunks(&stream, &cuts);
        let mut accounted = 0;
        for event in &events {
            match event {
                SplitEvent::Document(bytes) => {
                    accounted += bytes.len();
                    let _ = Message::parse(bytes);
                }
                SplitEvent::Discarded { bytes, .. } => accounted += bytes,
            }
        }
        prop_assert!(accounted <= stream.len());
        let _ = Message::parse(&stream);
    }

    #[test]
    fn every_parsed_tree_can_be_written_back(
        body in prop::collection::vec(
            prop_oneof![
                4 => prop::sample::select(b"<>/?!-[]'\"= aAV:_.\r\n&;#x3C".to_vec()),
                1 => any::<u8>(),
            ],
            0..120,
        ),
    ) {
        let mut input = b"<R a='1'>".to_vec();
        input.extend_from_slice(&body);
        input.extend_from_slice(b"</R>");
        if let Ok(message) = Message::parse(&input) {
            let rebuilt = Message::from_element(message.root().clone()).unwrap();
            let reparsed = Message::parse(rebuilt.as_bytes()).unwrap();
            prop_assert_eq!(reparsed.root(), message.root());
        }
    }
}
