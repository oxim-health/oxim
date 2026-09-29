//! The lab pipeline steps.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use oxim_core::{EngineError, Filter, MessageContext, StepConfig, StepError, Transformer};
use oxim_model::{ClinicalContent, CodeableConcept, OrderControl, OrderGroup, ResultGroup};
use tracing::{debug, warn};

use crate::cache::{CacheError, CachedOrder, CachedTest, OrderCache, TestStatus, specimen_id};
use crate::environment::LabEnvironment;
use crate::routing::Routing;

fn failure(step: &'static str) -> impl Fn(CacheError) -> StepError {
    move |e| StepError::new(step, e.to_string())
}

fn flag(step: &StepConfig, key: &str, default: bool) -> Result<bool, EngineError> {
    match step.settings.get(key) {
        None => Ok(default),
        Some(serde_json::Value::Bool(value)) => Ok(*value),
        Some(_) => Err(EngineError::Config(format!(
            "step {:?}: {key:?} must be true or false",
            step.kind
        ))),
    }
}

fn optional_text(step: &StepConfig, key: &str) -> Result<Option<String>, EngineError> {
    match step.settings.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(text)) if !text.trim().is_empty() => {
            Ok(Some(text.trim().to_owned()))
        }
        Some(_) => Err(EngineError::Config(format!(
            "step {:?}: {key:?} must be non-empty text",
            step.kind
        ))),
    }
}

/// Whether two concepts share a code.
fn shares_code(a: &CodeableConcept, b: &CodeableConcept) -> bool {
    a.codings
        .iter()
        .any(|x| b.codings.iter().any(|y| x.code == y.code))
}

/// The tests one device performs.
#[derive(Debug, Clone)]
struct DeviceTests {
    device: String,
    routing: Arc<Routing>,
    environment: LabEnvironment,
}

impl DeviceTests {
    fn from_step(step: &StepConfig, environment: &LabEnvironment) -> Result<Self, EngineError> {
        Ok(Self {
            device: step.text("device")?.trim().to_owned(),
            routing: environment.routing(step.text("routing")?)?,
            environment: environment.clone(),
        })
    }

    /// Whether the device performs `test` for this tube: the device the
    /// cache assigned it to (load balancing), or else any device the
    /// routing table lists for it.
    fn performs(&self, test: &CodeableConcept, cached: Option<&CachedOrder>) -> bool {
        if let Some(order) = cached
            && let Some(entry) = order
                .tests
                .iter()
                .find(|entry| test.codings.iter().any(|coding| coding.code == entry.code))
            && let Some(device) = &entry.device
        {
            return *device == self.device;
        }
        self.routing.routes(test, &self.device)
    }

    fn cached(
        &self,
        group: &OrderGroup,
        step: &'static str,
    ) -> Result<Option<CachedOrder>, StepError> {
        let Some(id) = specimen_id(group) else {
            return Ok(None);
        };
        let cache = self.environment.cache().map_err(failure(step))?;
        cache.get(id).map_err(failure(step))
    }

    /// Whether the device should see the group: it lists a test the device
    /// performs, or it lists no tests (such as "cancel the whole order") and
    /// the device performs one of the specimen's cached tests.
    fn concerns(&self, group: &OrderGroup, step: &'static str) -> Result<bool, StepError> {
        let cached = self.cached(group, step)?;
        if !group.order.tests.is_empty() {
            return Ok(group
                .order
                .tests
                .iter()
                .any(|test| self.performs(test, cached.as_ref())));
        }
        Ok(cached.as_ref().is_some_and(|order| {
            order
                .tests
                .iter()
                .any(|test| self.performs(&test.test, cached.as_ref()))
        }))
    }
}

/// How `cache-orders` chooses among several devices that perform a test.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Balance {
    /// The first device listed for the test in the routing table.
    #[default]
    Primary,
    /// The listed devices in turn, per test code.
    RoundRobin,
    /// The device with the fewest open tests in the cache.
    LeastLoaded,
}

impl Balance {
    fn parse(step: &StepConfig) -> Result<Self, EngineError> {
        match optional_text(step, "balance")?.as_deref() {
            None | Some("primary") => Ok(Self::Primary),
            Some("round_robin") => Ok(Self::RoundRobin),
            Some("least_loaded") => Ok(Self::LeastLoaded),
            Some(other) => Err(EngineError::Config(format!(
                "step {:?}: balance must be primary, round_robin or least_loaded, not {other:?}",
                step.kind
            ))),
        }
    }
}

/// Assigns open tests without a device to one of the devices that perform
/// them.
#[derive(Debug)]
struct Assigner {
    routing: Arc<Routing>,
    balance: Balance,
    turns: std::sync::Mutex<std::collections::HashMap<String, usize>>,
}

impl Assigner {
    fn assign(
        &self,
        cache: &OrderCache,
        order: &CachedOrder,
        load: &mut Option<std::collections::HashMap<String, u64>>,
        now: oxim_model::Timestamp,
    ) -> Result<usize, CacheError> {
        let mut assigned = 0;
        for test in order
            .tests
            .iter()
            .filter(|test| test.device.is_none() && test.status.is_open())
        {
            let devices = self.routing.devices(&test.test);
            let chosen = match (self.balance, devices.as_slice()) {
                (_, []) => continue,
                (_, [only]) => *only,
                (Balance::Primary, [first, ..]) => *first,
                (Balance::RoundRobin, _) => {
                    let mut turns = self.turns.lock().map_err(|_| CacheError::Unavailable)?;
                    let turn = turns.entry(test.code.clone()).or_default();
                    let chosen = devices[*turn % devices.len()];
                    *turn = turn.wrapping_add(1);
                    chosen
                }
                (Balance::LeastLoaded, _) => {
                    if load.is_none() {
                        *load = Some(cache.open_tests_by_device()?);
                    }
                    let counts = load.as_ref();
                    let open =
                        |device: &str| counts.and_then(|c| c.get(device)).copied().unwrap_or(0);
                    // Ties go to the device listed first.
                    let mut best = devices[0];
                    for device in &devices[1..] {
                        if open(device) < open(best) {
                            best = device;
                        }
                    }
                    best
                }
            };
            if cache.assign(&order.specimen_id, &test.code, chosen, now)? {
                assigned += 1;
                if let Some(counts) = load.as_mut() {
                    *counts.entry(chosen.to_owned()).or_default() += 1;
                }
            }
        }
        Ok(assigned)
    }
}

const CACHE_ORDERS: &str = "cache-orders";

/// Files orders in the order cache.
///
/// ```yaml
/// transformers:
///   - type: cache-orders
///     require_specimen: false   # true: an order without a specimen ID is an error
///     routing: routing.csv      # optional: assign each test to one device
///     balance: least_loaded     # primary (default), round_robin or least_loaded
/// ```
///
/// Other content passes unchanged. Orders are filed by specimen identifier
/// (see [`OrderCache::apply`](crate::OrderCache::apply)); an order without
/// one cannot be matched to a tube and is skipped with a warning, or marks
/// the message as errored with `require_specimen: true`.
///
/// With `routing`, each open test without a device is assigned to one of
/// the devices that perform it ([`Balance`]); `select-tests` and
/// `has-tests-for` then send it to that device only. Without `routing`,
/// every device that performs a test receives it. A device that asks for a
/// tube in a host query is offered every test it can perform, whatever the
/// assignment, since the tube is in front of it.
#[derive(Debug)]
pub struct CacheOrders {
    environment: LabEnvironment,
    require_specimen: bool,
    assigner: Option<Assigner>,
}

impl CacheOrders {
    /// Builds the step from its configuration.
    pub fn from_step(step: &StepConfig, environment: &LabEnvironment) -> Result<Self, EngineError> {
        let balance = Balance::parse(step)?;
        let assigner = optional_text(step, "routing")?
            .map(|name| environment.routing(&name))
            .transpose()?
            .map(|routing| Assigner {
                routing,
                balance,
                turns: std::sync::Mutex::new(std::collections::HashMap::new()),
            });
        if assigner.is_none() && step.settings.contains_key("balance") {
            return Err(EngineError::Config(format!(
                "step {CACHE_ORDERS:?}: \"balance\" needs \"routing\""
            )));
        }
        Ok(Self {
            environment: environment.clone(),
            require_specimen: flag(step, "require_specimen", false)?,
            assigner,
        })
    }
}

impl Transformer for CacheOrders {
    fn apply(&self, context: &mut MessageContext) -> Result<(), StepError> {
        let Some(ClinicalContent::Orders { groups }) = &context.clinical else {
            return Ok(());
        };
        let cache = self.environment.cache().map_err(failure(CACHE_ORDERS))?;
        let now = context.envelope.received_at;
        let mut load = None;
        for group in groups {
            match cache.apply(group, now) {
                Ok(outcome) => {
                    let mut assigned = 0;
                    if let Some(assigner) = &self.assigner
                        && let Some(order) = cache
                            .get(&outcome.specimen_id)
                            .map_err(failure(CACHE_ORDERS))?
                    {
                        assigned = assigner
                            .assign(&cache, &order, &mut load, now)
                            .map_err(failure(CACHE_ORDERS))?;
                    }
                    debug!(
                        specimen = %outcome.specimen_id,
                        added = outcome.added,
                        cancelled = outcome.cancelled,
                        assigned,
                        "cached order"
                    );
                }
                Err(CacheError::MissingSpecimen) if !self.require_specimen => warn!(
                    message = %context.envelope.id,
                    "an order without a specimen identifier was not cached"
                ),
                Err(e) => return Err(failure(CACHE_ORDERS)(e)),
            }
        }
        Ok(())
    }
}

const ANSWER_QUERY: &str = "answer-query";

/// Answers a device's host query from the order cache.
///
/// ```yaml
/// transformers:
///   - type: answer-query
///     device: chem-1              # optional; recorded and used for routing
///     routing: routing.csv        # optional; only tests routed to the device
///     include_resulted: false     # true: also offer tests that have results
///     mark_sent: true             # record the offered tests as sent
/// ```
///
/// A `Query` becomes `Orders` holding, for each queried specimen found in
/// the cache, the open tests (pending or sent) that the device performs and
/// that the query asked for (all when it asked for all). Specimens that are
/// unknown or have nothing left for the device are left out, so an encoder
/// such as `astm-query-response` answers "no information" for them. Other
/// content passes unchanged.
#[derive(Debug, Clone)]
pub struct AnswerQuery {
    environment: LabEnvironment,
    device: Option<String>,
    routing: Option<Arc<Routing>>,
    include_resulted: bool,
    mark_sent: bool,
}

impl AnswerQuery {
    /// Builds the step from its configuration.
    pub fn from_step(step: &StepConfig, environment: &LabEnvironment) -> Result<Self, EngineError> {
        let device = optional_text(step, "device")?;
        let routing = optional_text(step, "routing")?
            .map(|name| environment.routing(&name))
            .transpose()?;
        if routing.is_some() && device.is_none() {
            return Err(EngineError::Config(format!(
                "step {ANSWER_QUERY:?}: \"routing\" needs \"device\""
            )));
        }
        Ok(Self {
            environment: environment.clone(),
            device,
            routing,
            include_resulted: flag(step, "include_resulted", false)?,
            mark_sent: flag(step, "mark_sent", true)?,
        })
    }

    fn routed(&self, test: &CodeableConcept) -> bool {
        match (&self.routing, &self.device) {
            (Some(routing), Some(device)) => routing.routes(test, device),
            _ => true,
        }
    }
}

impl Transformer for AnswerQuery {
    fn apply(&self, context: &mut MessageContext) -> Result<(), StepError> {
        let Some(ClinicalContent::Query { query, .. }) = &context.clinical else {
            return Ok(());
        };
        let query = query.clone();
        let cache = self.environment.cache().map_err(failure(ANSWER_QUERY))?;
        let now = context.envelope.received_at;
        let requested = |test: &CachedTest| {
            query.all_tests
                || query.tests.is_empty()
                || query
                    .tests
                    .iter()
                    .any(|asked| shares_code(asked, &test.test))
        };
        let offered = |test: &CachedTest| {
            let status = test.status.is_open()
                || (self.include_resulted && test.status == TestStatus::Resulted);
            status && self.routed(&test.test) && requested(test)
        };
        let mut groups = Vec::new();
        let mut seen = BTreeSet::new();
        for id in &query.specimen_ids {
            if !seen.insert(id.as_str()) {
                continue;
            }
            let Some(order) = cache.get(id).map_err(failure(ANSWER_QUERY))? else {
                debug!(specimen = %id, "host query for an unknown specimen");
                continue;
            };
            let mut group = order.to_group(offered);
            if group.order.tests.is_empty() {
                continue;
            }
            group.order.control = Some(OrderControl::New);
            if self.mark_sent {
                let open: Vec<&str> = order
                    .tests
                    .iter()
                    .filter(|test| test.status.is_open() && offered(test))
                    .map(|test| test.code.as_str())
                    .collect();
                cache
                    .set_status(id, &open, TestStatus::Sent, self.device.as_deref(), now)
                    .map_err(failure(ANSWER_QUERY))?;
            }
            groups.push(group);
        }
        debug!(
            specimens = query.specimen_ids.len(),
            answered = groups.len(),
            "answered host query"
        );
        context.clinical = Some(ClinicalContent::Orders { groups });
        Ok(())
    }
}

const RECORD_RESULTS: &str = "record-results";

fn result_specimen(group: &ResultGroup) -> Option<&str> {
    group
        .specimen
        .as_ref()
        .and_then(|specimen| specimen.identifiers.first())
        .map(|identifier| identifier.value.as_str())
        .or_else(|| {
            group
                .order
                .as_ref()
                .and_then(|order| order.specimen_ids.first())
                .map(String::as_str)
        })
}

/// Marks cached tests as resulted when their results arrive.
///
/// ```yaml
/// transformers:
///   - type: record-results
///     device: chem-1   # optional; recorded as the device that performed the tests
/// ```
///
/// A test is resulted when an observation or the order of its result group
/// shares a code with it, so an ordered panel is closed by the result
/// message that reports it. Results for specimens that are not cached are
/// ignored. Other content passes unchanged; results are never changed.
#[derive(Debug, Clone)]
pub struct RecordResults {
    environment: LabEnvironment,
    device: Option<String>,
}

impl RecordResults {
    /// Builds the step from its configuration.
    pub fn from_step(step: &StepConfig, environment: &LabEnvironment) -> Result<Self, EngineError> {
        Ok(Self {
            environment: environment.clone(),
            device: optional_text(step, "device")?,
        })
    }
}

impl Transformer for RecordResults {
    fn apply(&self, context: &mut MessageContext) -> Result<(), StepError> {
        let Some(ClinicalContent::Results { groups, .. }) = &context.clinical else {
            return Ok(());
        };
        let mut resulted: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for group in groups {
            let fallback = result_specimen(group);
            for observation in &group.observations {
                let Some(specimen) = observation.specimen_id.as_deref().or(fallback) else {
                    continue;
                };
                let codes = resulted.entry(specimen).or_default();
                codes.extend(observation.code.codings.iter().map(|c| c.code.as_str()));
            }
            if let (Some(order), Some(specimen)) = (&group.order, fallback)
                && !group.observations.is_empty()
            {
                let codes = resulted.entry(specimen).or_default();
                for test in &order.tests {
                    codes.extend(test.codings.iter().map(|c| c.code.as_str()));
                }
            }
        }
        if resulted.is_empty() {
            return Ok(());
        }
        let cache = self.environment.cache().map_err(failure(RECORD_RESULTS))?;
        for (specimen, codes) in resulted {
            let codes: Vec<&str> = codes.into_iter().collect();
            let changed = cache
                .set_status(
                    specimen,
                    &codes,
                    TestStatus::Resulted,
                    self.device.as_deref(),
                    context.envelope.received_at,
                )
                .map_err(failure(RECORD_RESULTS))?;
            debug!(%specimen, changed, "recorded results");
        }
        Ok(())
    }
}

const SELECT_TESTS: &str = "select-tests";

/// Merges order groups for the same specimen, patient, control and
/// priority into one, since analyzers expect one order per tube. The first
/// group's order details are kept.
fn merge_by_specimen(groups: Vec<OrderGroup>) -> Vec<OrderGroup> {
    let mut merged: Vec<OrderGroup> = Vec::with_capacity(groups.len());
    for group in groups {
        let same = |other: &OrderGroup| {
            specimen_id(other).is_some()
                && specimen_id(other) == specimen_id(&group)
                && other.patient == group.patient
                && other.order.control == group.order.control
                && other.order.priority == group.order.priority
                && !other.order.tests.is_empty()
                && !group.order.tests.is_empty()
        };
        match merged.iter_mut().find(|other| same(other)) {
            Some(target) => {
                for test in group.order.tests {
                    if !target.order.tests.contains(&test) {
                        target.order.tests.push(test);
                    }
                }
            }
            None => merged.push(group),
        }
    }
    merged
}

/// Keeps only the tests one device performs, for sending orders to that
/// device (a worklist download).
///
/// ```yaml
/// destinations:
///   - id: chem-1
///     type: astm-tcp
///     filters:
///       - {type: has-tests-for, device: chem-1, routing: routing.csv}
///     transformers:
///       - {type: select-tests, device: chem-1, routing: routing.csv}
///     encoder: {type: astm-orders}
/// ```
///
/// Groups for the same tube are merged into one order. Order groups left
/// without tests are dropped, except groups that listed no tests to begin
/// with (such as "cancel the whole order"), which are
/// kept when the device performs one of the specimen's cached tests. With
/// `mark_sent` (the default) the selected tests of new and changed orders
/// are recorded as sent to the device. Other content passes unchanged.
#[derive(Debug, Clone)]
pub struct SelectTests {
    target: DeviceTests,
    mark_sent: bool,
}

impl SelectTests {
    /// Builds the step from its configuration.
    pub fn from_step(step: &StepConfig, environment: &LabEnvironment) -> Result<Self, EngineError> {
        Ok(Self {
            target: DeviceTests::from_step(step, environment)?,
            mark_sent: flag(step, "mark_sent", true)?,
        })
    }
}

impl Transformer for SelectTests {
    fn apply(&self, context: &mut MessageContext) -> Result<(), StepError> {
        let Some(ClinicalContent::Orders { groups }) = &mut context.clinical else {
            return Ok(());
        };
        let mut kept = Vec::with_capacity(groups.len());
        for mut group in std::mem::take(groups) {
            if group.order.tests.is_empty() {
                if self.target.concerns(&group, SELECT_TESTS)? {
                    kept.push(group);
                }
                continue;
            }
            let cached = self.target.cached(&group, SELECT_TESTS)?;
            group
                .order
                .tests
                .retain(|test| self.target.performs(test, cached.as_ref()));
            if !group.order.tests.is_empty() {
                kept.push(group);
            }
        }
        let kept = merge_by_specimen(kept);
        if self.mark_sent {
            let cache = self
                .target
                .environment
                .cache()
                .map_err(failure(SELECT_TESTS))?;
            for group in &kept {
                if group.order.control == Some(OrderControl::Cancel) {
                    continue;
                }
                let Some(id) = specimen_id(group) else {
                    continue;
                };
                let codes: Vec<&str> = group
                    .order
                    .tests
                    .iter()
                    .flat_map(|test| test.codings.iter().map(|coding| coding.code.as_str()))
                    .collect();
                // The device is recorded by assignment, host queries and
                // results; several devices may be offered the same test here.
                cache
                    .set_status(
                        id,
                        &codes,
                        TestStatus::Sent,
                        None,
                        context.envelope.received_at,
                    )
                    .map_err(failure(SELECT_TESTS))?;
            }
        }
        *groups = kept;
        Ok(())
    }
}

/// Keeps order messages that concern one device: they list a test the
/// device performs, or cancel a whole order the device has tests of.
///
/// ```yaml
/// filters:
///   - {type: has-tests-for, device: hema-1, routing: routing.csv, negate: false}
/// ```
///
/// Messages without orders never match.
#[derive(Debug, Clone)]
pub struct HasTestsFor {
    target: DeviceTests,
    negate: bool,
}

impl HasTestsFor {
    /// Builds the step from its configuration.
    pub fn from_step(step: &StepConfig, environment: &LabEnvironment) -> Result<Self, EngineError> {
        Ok(Self {
            target: DeviceTests::from_step(step, environment)?,
            negate: flag(step, "negate", false)?,
        })
    }
}

impl Filter for HasTestsFor {
    fn accept(&self, context: &MessageContext) -> Result<bool, StepError> {
        let matched = match &context.clinical {
            Some(ClinicalContent::Orders { groups }) => {
                let mut matched = false;
                for group in groups {
                    if self.target.concerns(group, "has-tests-for")? {
                        matched = true;
                        break;
                    }
                }
                matched
            }
            _ => false,
        };
        Ok(matched != self.negate)
    }
}
