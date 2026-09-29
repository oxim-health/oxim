//! Laboratory workflows for OXIM: an order cache, host query answers and
//! test routing between a LIS and its analyzers.
//!
//! Analyzers ask "what should I run on tube S123?" (a host query) and expect
//! an answer within seconds, while the LIS may be slow or down. OXIM keeps
//! every order it forwards in an [`OrderCache`] and answers queries itself:
//!
//! ```text
//! LIS ──orders──▶ OXIM ── cache-orders ──▶ order cache
//!                  │                          ▲   │
//!                  └─ select-tests ─▶ analyzer │   │ answer-query
//!                                    (worklist) │   ▼
//! analyzer ──host query──────────────────────▶ OXIM ──reply──▶ analyzer
//! analyzer ──results──▶ OXIM ── record-results ──▶ LIS
//! ```
//!
//! | Type | Kind | Purpose |
//! |---|---|---|
//! | `cache-orders` | transformer | files `Orders` in the cache (new, add, replace, cancel) |
//! | `answer-query` | transformer | turns a `Query` into the cached `Orders` for the queried specimens |
//! | `record-results` | transformer | marks cached tests as resulted |
//! | `select-tests` | transformer | keeps only the tests a device performs |
//! | `has-tests-for` | filter | keeps orders that concern a device |
//!
//! Tests are routed to devices by a [`Routing`] table. The steps work on the
//! normalized model (`normalize: true`), never change results, and never
//! interpret clinical values (ADR 0011).
//!
//! A host query channel for an ASTM analyzer:
//!
//! ```yaml
//! id: chem-1-queries
//! source:
//!   type: astm-tcp
//!   data_type: astm
//!   normalize: true
//!   response:
//!     mode: pipeline
//!     encoder: {type: astm-query-response}
//!   settings: {listen: 0.0.0.0:5001}
//! transformers:
//!   - {type: map-observations, table: chem-1-to-lis.csv}
//!   - {type: answer-query, device: chem-1, routing: routing.csv}
//!   - {type: record-results, device: chem-1}
//!   - {type: map-observations, table: lis-to-chem-1.csv}
//! ```
//!
//! The cache is derived data: deleting it loses nothing that reprocessing
//! the stored order messages cannot rebuild.

mod cache;
mod environment;
mod routing;
mod steps;

use std::sync::Arc;

use oxim_core::{Filter, Registry, Transformer};

pub use cache::{
    ApplyOutcome, CacheError, CacheResult, CachedOrder, CachedTest, OrderCache, TestStatus,
    specimen_id, test_code,
};
pub use environment::LabEnvironment;
pub use routing::{Routing, RoutingError};
pub use steps::{AnswerQuery, CacheOrders, HasTestsFor, RecordResults, SelectTests};

/// Registers the lab steps with an engine registry.
pub fn register(registry: &mut Registry, environment: LabEnvironment) {
    let cache_orders = environment.clone();
    let answer_query = environment.clone();
    let record_results = environment.clone();
    let select_tests = environment.clone();
    let has_tests_for = environment;
    registry
        .add_transformer("cache-orders", move |step| {
            Ok(Arc::new(CacheOrders::from_step(step, &cache_orders)?) as Arc<dyn Transformer>)
        })
        .add_transformer("answer-query", move |step| {
            Ok(Arc::new(AnswerQuery::from_step(step, &answer_query)?) as Arc<dyn Transformer>)
        })
        .add_transformer("record-results", move |step| {
            Ok(Arc::new(RecordResults::from_step(step, &record_results)?) as Arc<dyn Transformer>)
        })
        .add_transformer("select-tests", move |step| {
            Ok(Arc::new(SelectTests::from_step(step, &select_tests)?) as Arc<dyn Transformer>)
        })
        .add_filter("has-tests-for", move |step| {
            Ok(Arc::new(HasTestsFor::from_step(step, &has_tests_for)?) as Arc<dyn Filter>)
        });
}
