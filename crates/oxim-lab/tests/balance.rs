//! Load balancing: tests that several analyzers perform are assigned to one
//! of them, and worklists follow the assignment.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::Arc;

use oxim_core::{Document, MessageContext, StepConfig, Transformer};
use oxim_lab::{AnswerQuery, CacheOrders, LabEnvironment, OrderCache, Routing, SelectTests};
use oxim_model::{
    ChannelId, ClinicalContent, CodeableConcept, Coding, ConnectorId, DataType, Envelope,
    Identifier, MessageId, Order, OrderControl, OrderGroup, Specimen, SpecimenQuery, Timestamp,
};

fn environment() -> (LabEnvironment, Arc<OrderCache>) {
    let cache = Arc::new(OrderCache::open_in_memory().unwrap());
    let routing = Routing::new()
        .with("GLU", "chem-1")
        .with("GLU", "chem-2")
        .with("HGB", "hema-1");
    (
        LabEnvironment::with_cache(cache.clone()).with_routing("routing.csv", routing),
        cache,
    )
}

fn context(content: ClinicalContent, n: u64) -> MessageContext {
    let envelope = Envelope::new(
        MessageId::from_parts(1_790_000_000_000 + n, u128::from(n)),
        ChannelId::new("lis-orders").unwrap(),
        ConnectorId::new("source").unwrap(),
        Timestamp::from_unix_millis(1_790_000_000_000 + i64::try_from(n).unwrap()).unwrap(),
        DataType::Json,
        Vec::new(),
    );
    MessageContext {
        envelope,
        document: Document::Raw(Vec::new()),
        clinical: Some(content),
        variables: BTreeMap::new(),
        response: None,
    }
}

fn order(specimen: &str, tests: &[&str]) -> ClinicalContent {
    ClinicalContent::Orders {
        groups: vec![OrderGroup {
            patient: None,
            specimen: Some(Specimen {
                identifiers: vec![Identifier::new(specimen)],
                ..Specimen::default()
            }),
            order: Order {
                tests: tests
                    .iter()
                    .map(|code| CodeableConcept::from_coding(Coding::new(*code)))
                    .collect(),
                control: Some(OrderControl::New),
                ..Order::default()
            },
        }],
    }
}

fn device_of(cache: &OrderCache, specimen: &str, code: &str) -> Option<String> {
    cache
        .get(specimen)
        .unwrap()
        .unwrap()
        .tests
        .into_iter()
        .find(|test| test.code == code)
        .unwrap()
        .device
}

fn step(kind: &str, settings: serde_json::Value) -> StepConfig {
    let mut step = StepConfig::new(kind);
    if let serde_json::Value::Object(map) = settings {
        step.settings = map;
    }
    step
}

fn cache_orders(environment: &LabEnvironment, balance: Option<&str>) -> CacheOrders {
    let mut settings = serde_json::json!({"routing": "routing.csv"});
    if let Some(balance) = balance {
        settings["balance"] = balance.into();
    }
    CacheOrders::from_step(&step("cache-orders", settings), environment).unwrap()
}

fn assigned(balance: Option<&str>) -> Vec<String> {
    let (environment, cache) = environment();
    let step = cache_orders(&environment, balance);
    let mut devices = Vec::new();
    for (n, specimen) in ["S1", "S2", "S3", "S4"].iter().enumerate() {
        let mut message = context(order(specimen, &["GLU", "HGB"]), n as u64);
        step.apply(&mut message).unwrap();
        devices.push(device_of(&cache, specimen, "GLU").unwrap());
        assert_eq!(device_of(&cache, specimen, "HGB").as_deref(), Some("hema-1"));
    }
    devices
}

#[test]
fn strategies_spread_tests_over_devices() {
    assert_eq!(assigned(None), ["chem-1", "chem-1", "chem-1", "chem-1"]);
    assert_eq!(
        assigned(Some("round_robin")),
        ["chem-1", "chem-2", "chem-1", "chem-2"]
    );
    assert_eq!(
        assigned(Some("least_loaded")),
        ["chem-1", "chem-2", "chem-1", "chem-2"]
    );
}

#[test]
fn worklists_follow_the_assignment_and_queries_reassign() {
    let (environment, cache) = environment();
    let caching = cache_orders(&environment, Some("round_robin"));
    for (n, specimen) in ["S1", "S2"].iter().enumerate() {
        caching
            .apply(&mut context(order(specimen, &["GLU"]), n as u64))
            .unwrap();
    }
    assert_eq!(device_of(&cache, "S1", "GLU").as_deref(), Some("chem-1"));
    assert_eq!(device_of(&cache, "S2", "GLU").as_deref(), Some("chem-2"));

    let select = |device: &str| {
        SelectTests::from_step(
            &step(
                "select-tests",
                serde_json::json!({"device": device, "routing": "routing.csv"}),
            ),
            &environment,
        )
        .unwrap()
    };
    let tests_for = |device: &str, specimen: &str| {
        let mut message = context(order(specimen, &["GLU"]), 10);
        select(device).apply(&mut message).unwrap();
        let Some(ClinicalContent::Orders { groups }) = message.clinical else {
            panic!("orders expected");
        };
        groups.len()
    };
    assert_eq!(tests_for("chem-1", "S1"), 1);
    assert_eq!(tests_for("chem-2", "S1"), 0);
    assert_eq!(tests_for("chem-2", "S2"), 1);
    // Sending does not move the assignment.
    assert_eq!(device_of(&cache, "S1", "GLU").as_deref(), Some("chem-1"));

    // The tube of S1 ends up at chem-2, which asks for it: it gets GLU,
    // and the assignment follows the tube.
    let answer = AnswerQuery::from_step(
        &step(
            "answer-query",
            serde_json::json!({"device": "chem-2", "routing": "routing.csv"}),
        ),
        &environment,
    )
    .unwrap();
    let mut query = context(
        ClinicalContent::Query {
            device: None,
            query: SpecimenQuery {
                specimen_ids: vec!["S1".into()],
                all_tests: true,
                ..SpecimenQuery::default()
            },
        },
        20,
    );
    answer.apply(&mut query).unwrap();
    let Some(ClinicalContent::Orders { groups }) = query.clinical else {
        panic!("orders expected");
    };
    assert_eq!(groups[0].order.tests.len(), 1);
    assert_eq!(device_of(&cache, "S1", "GLU").as_deref(), Some("chem-2"));
}

#[test]
fn balance_needs_routing() {
    let (environment, _) = environment();
    let error = CacheOrders::from_step(
        &step("cache-orders", serde_json::json!({"balance": "round_robin"})),
        &environment,
    )
    .unwrap_err();
    assert!(error.to_string().contains("routing"), "{error}");
    let error = CacheOrders::from_step(
        &step(
            "cache-orders",
            serde_json::json!({"routing": "routing.csv", "balance": "random"}),
        ),
        &environment,
    )
    .unwrap_err();
    assert!(error.to_string().contains("least_loaded"), "{error}");
}
