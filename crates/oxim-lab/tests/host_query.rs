//! Orders from a LIS are cached, pushed to the analyzers that perform them
//! and used to answer the analyzers' host queries; results close them.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use oxim_core::{
    ChannelConfig, DestinationConnector, Engine, EngineOptions, Registry, Reply, SendError,
    SourceConnector, SourceContext, SubmitInfo, SystemClock, async_trait,
};
use oxim_lab::{LabEnvironment, OrderCache, Routing, TestStatus};
use oxim_model::MessageStatus;
use oxim_store::{Delivery, SqliteStore};
use tokio::sync::{Mutex, mpsc, oneshot};

type Request = (Vec<u8>, oneshot::Sender<Reply>);

/// A source fed through an in-process queue; it waits for each reply.
#[derive(Debug)]
struct RequestSource {
    inbox: Mutex<mpsc::Receiver<Request>>,
}

#[async_trait]
impl SourceConnector for RequestSource {
    async fn run(&self, context: SourceContext) -> Result<(), oxim_core::ConnectorError> {
        let mut inbox = self.inbox.lock().await;
        loop {
            tokio::select! {
                () = context.cancelled() => return Ok(()),
                next = inbox.recv() => {
                    let Some((raw, reply)) = next else { return Ok(()) };
                    let answer = context.request(raw, SubmitInfo::default()).await.unwrap();
                    let _ = reply.send(answer);
                }
            }
        }
    }
}

/// Collects what is sent to it, by destination name.
#[derive(Debug)]
struct Capture {
    name: String,
    sent: Arc<StdMutex<Vec<(String, String)>>>,
}

#[async_trait]
impl DestinationConnector for Capture {
    async fn send(&self, delivery: &Delivery) -> Result<Option<Vec<u8>>, SendError> {
        let text = String::from_utf8_lossy(&delivery.payload).into_owned();
        self.sent.lock().unwrap().push((self.name.clone(), text));
        Ok(None)
    }
}

struct Lab {
    engine: Engine,
    inboxes: HashMap<&'static str, mpsc::Sender<Request>>,
    cache: Arc<OrderCache>,
    sent: Arc<StdMutex<Vec<(String, String)>>>,
}

async fn lab() -> Lab {
    let mut senders = HashMap::new();
    let mut receivers = HashMap::new();
    for name in ["orders", "chem-1", "hema-1", "immuno-1"] {
        let (tx, rx) = mpsc::channel(8);
        senders.insert(name, tx);
        receivers.insert(
            name.to_owned(),
            Arc::new(RequestSource {
                inbox: Mutex::new(rx),
            }),
        );
    }
    let cache = Arc::new(OrderCache::open_in_memory().unwrap());
    let sent = Arc::new(StdMutex::new(Vec::new()));
    let routing = Routing::new()
        .with("GLU", "chem-1")
        .with("CREA", "chem-1")
        .with("HGB", "hema-1")
        .with("TSH", "immuno-1");
    let environment =
        LabEnvironment::with_cache(cache.clone()).with_routing("routing.csv", routing);

    let mut registry = Registry::new();
    oxim_mapping::register(&mut registry);
    oxim_lab::register(&mut registry, environment);
    let captured = sent.clone();
    registry
        .add_source("request", move |config| {
            let inbox = config.settings["inbox"].as_str().unwrap();
            Ok(receivers[inbox].clone() as Arc<dyn SourceConnector>)
        })
        .add_destination("capture", move |config| {
            Ok(Arc::new(Capture {
                name: config.id.to_string(),
                sent: captured.clone(),
            }) as Arc<dyn DestinationConnector>)
        });
    let mut options = EngineOptions::default();
    options.idle_poll = Duration::from_millis(20);
    options.shutdown_grace = Duration::from_secs(2);
    let engine = Engine::start(
        Box::new(SqliteStore::open_in_memory().unwrap()),
        registry,
        Arc::new(SystemClock),
        options,
    )
    .await
    .unwrap();

    let device_destination = |device: &str| {
        format!(
            "  - id: {device}
    type: capture
    filters: [{{type: has-tests-for, device: {device}, routing: routing.csv}}]
    transformers: [{{type: select-tests, device: {device}, routing: routing.csv}}]
    encoder: {{type: astm-orders}}
"
        )
    };
    let orders = format!(
        "id: lis-orders
source:
  type: request
  data_type: hl7v2
  normalize: true
  response: {{mode: pipeline}}
  settings: {{inbox: orders}}
transformers:
  - {{type: cache-orders}}
destinations:
{}{}",
        device_destination("chem-1"),
        device_destination("hema-1")
    );
    engine
        .deploy(ChannelConfig::from_yaml(&orders).unwrap())
        .await
        .unwrap();
    for device in ["chem-1", "hema-1"] {
        let queries = format!(
            "id: {device}-queries
source:
  type: request
  data_type: astm
  normalize: true
  response:
    mode: pipeline
    encoder: {{type: astm-query-response, sender: OXIM}}
  settings: {{inbox: {device}}}
transformers:
  - {{type: answer-query, device: {device}, routing: routing.csv}}
  - {{type: record-results, device: {device}}}
"
        );
        engine
            .deploy(ChannelConfig::from_yaml(&queries).unwrap())
            .await
            .unwrap();
    }
    // An HL7 analyzer (IHE LAW): queries are answered with RSP^K11 that
    // carries the orders.
    let hl7 = "id: immuno-1-queries
source:
  type: request
  data_type: hl7v2
  normalize: true
  response:
    mode: pipeline
    encoder: {type: hl7v2-rsp-k11, include_orders: true}
  settings: {inbox: immuno-1}
transformers:
  - {type: answer-query, device: immuno-1, routing: routing.csv}
";
    engine
        .deploy(ChannelConfig::from_yaml(hl7).unwrap())
        .await
        .unwrap();
    Lab {
        engine,
        inboxes: senders,
        cache,
        sent,
    }
}

impl Lab {
    async fn ask(&self, inbox: &str, raw: &str) -> Reply {
        let (reply, answer) = oneshot::channel();
        self.inboxes[inbox]
            .send((raw.as_bytes().to_vec(), reply))
            .await
            .unwrap();
        answer.await.unwrap()
    }

    fn status(&self, specimen: &str, code: &str) -> TestStatus {
        let order = self.cache.get(specimen).unwrap().unwrap();
        order
            .tests
            .iter()
            .find(|test| test.code == code)
            .unwrap()
            .status
    }

    async fn wait_for_pushes(&self, count: usize) -> Vec<(String, String)> {
        for _ in 0..300 {
            let sent = self.sent.lock().unwrap().clone();
            if sent.len() >= count {
                return sent;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("expected {count} worklist downloads");
    }
}

const ORDER: &str = "MSH|^~\\&|LIS|LAB|OXIM|LAB|20260929100000||OML^O21|M1|P|2.5.1\r\
PID|1||P1001^^^LAB^MR||DOE^JANE||19800101|F\r\
ORC|NW|ORD1\rOBR|1|ORD1||GLU^Glucose^L\rSPM|1|S123||BLD\r\
ORC|NW|ORD1\rOBR|2|ORD1||CREA^Creatinine^L\rSPM|1|S123||BLD\r\
ORC|NW|ORD1\rOBR|3|ORD1||HGB^Hemoglobin^L\rSPM|1|S123||BLD\r\
ORC|NW|ORD1\rOBR|4|ORD1||TSH^Thyrotropin^L\rSPM|1|S123||BLD\r";

fn query(specimen: &str, tests: &str) -> String {
    format!("H|\\^&|||CHEM^1.0\rQ|1|^{specimen}||{tests}||||||||O\rL|1|N\r")
}

fn records(reply: &Reply) -> Vec<String> {
    let text = String::from_utf8(reply.data.clone().unwrap()).unwrap();
    text.split('\r')
        .filter(|record| !record.is_empty())
        .map(str::to_owned)
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn orders_are_cached_pushed_and_answered() {
    let lab = lab().await;
    let reply = lab.ask("orders", ORDER).await;
    assert_eq!(
        reply.status,
        MessageStatus::Transformed,
        "{:?}",
        reply.error
    );

    // Each analyzer receives only its own tests.
    let mut pushed = lab.wait_for_pushes(2).await;
    pushed.sort();
    assert_eq!(pushed[0].0, "chem-1");
    assert!(
        pushed[0].1.contains("|S123||^^^GLU\\^^^CREA|"),
        "{}",
        pushed[0].1
    );
    assert_eq!(pushed[1].0, "hema-1");
    assert!(pushed[1].1.contains("|S123||^^^HGB|"), "{}", pushed[1].1);
    assert!(!pushed[1].1.contains("GLU"));
    assert_eq!(lab.status("S123", "GLU"), TestStatus::Sent);

    // A host query for all tests is answered with the analyzer's tests.
    let reply = lab.ask("chem-1", &query("S123", "ALL")).await;
    assert_eq!(reply.status, MessageStatus::Completed, "{:?}", reply.error);
    let answer = records(&reply);
    assert!(answer[0].starts_with("H|\\^&|||OXIM|"), "{answer:?}");
    assert!(answer[1].starts_with("P|1|P1001|"), "{answer:?}");
    assert!(
        answer[2].starts_with("O|1|S123||^^^GLU\\^^^CREA|"),
        "{answer:?}"
    );
    assert!(answer[2].ends_with("|Q"), "{answer:?}");
    assert_eq!(answer[3], "L|1|N");

    // A query for one test gets that test; an unknown tube gets "no
    // information".
    let answer = records(&lab.ask("chem-1", &query("S123", "^^^CREA")).await);
    assert!(answer[2].starts_with("O|1|S123||^^^CREA|"), "{answer:?}");
    let answer = records(&lab.ask("chem-1", &query("S999", "ALL")).await);
    assert_eq!(answer.last().unwrap(), "L|1|I");

    // Results close the tests; resulted tests are no longer offered.
    let results =
        "H|\\^&|||CHEM^1.0\rP|1\rO|1|S123||^^^GLU|R\rR|1|^^^GLU|5.4|mmol/L||N||F\rL|1|N\r";
    let reply = lab.ask("chem-1", results).await;
    assert_ne!(reply.status, MessageStatus::Error, "{:?}", reply.error);
    assert_eq!(lab.status("S123", "GLU"), TestStatus::Resulted);
    assert_eq!(lab.status("S123", "CREA"), TestStatus::Sent);
    let answer = records(&lab.ask("chem-1", &query("S123", "ALL")).await);
    assert!(answer[2].starts_with("O|1|S123||^^^CREA|"), "{answer:?}");

    // The hematology analyzer only sees hemoglobin.
    let answer = records(&lab.ask("hema-1", &query("S123", "ALL")).await);
    assert!(answer[2].starts_with("O|1|S123||^^^HGB|"), "{answer:?}");
    assert_eq!(
        lab.cache.get("S123").unwrap().unwrap().tests[2]
            .device
            .as_deref(),
        Some("hema-1")
    );

    // The HL7 analyzer queries with QBP^Q11 and gets RSP^K11 with its test.
    let reply = lab
        .ask(
            "immuno-1",
            "MSH|^~\\&|IMMUNO|LAB|OXIM|LAB|20260929110000||QBP^Q11^QBP_Q11|Q7|P|2.5.1\r\
QPD|WOS^Work Order Step^IHE_LABTF|T7|S123\rRCP|I||R\r",
        )
        .await;
    let text = String::from_utf8(reply.data.unwrap()).unwrap();
    assert!(text.contains("\rMSA|AA|Q7\rQAK|T7|OK|"), "{text}");
    assert!(
        text.contains("\rSPM|1|S123||BLD\rSAC|||S123\rORC|NW|ORD1\rOBR|1|ORD1||TSH^Thyrotropin^L"),
        "{text}"
    );
    assert!(!text.contains("GLU"), "{text}");
    let reply = lab
        .ask(
            "immuno-1",
            "MSH|^~\\&|IMMUNO|LAB|OXIM|LAB|20260929110000||QBP^Q11^QBP_Q11|Q8|P|2.5.1\r\
QPD|WOS^Work Order Step^IHE_LABTF|T8|S999\rRCP|I||R\r",
        )
        .await;
    let text = String::from_utf8(reply.data.unwrap()).unwrap();
    assert!(text.contains("\rQAK|T8|NF|"), "{text}");

    // Cancelling the whole order reaches both analyzers and empties the
    // answers.
    let cancel = ORDER
        .replace("OML^O21|M1", "OML^O21|M2")
        .replace(
            "ORC|NW|ORD1\rOBR|2|ORD1||CREA^Creatinine^L\rSPM|1|S123||BLD\r",
            "",
        )
        .replace(
            "ORC|NW|ORD1\rOBR|3|ORD1||HGB^Hemoglobin^L\rSPM|1|S123||BLD\r",
            "",
        )
        .replace("ORC|NW|ORD1\rOBR|1|ORD1||GLU^Glucose^L\r", "ORC|CA|ORD1\r");
    let reply = lab.ask("orders", &cancel).await;
    assert_eq!(
        reply.status,
        MessageStatus::Transformed,
        "{:?}",
        reply.error
    );
    let pushed = lab.wait_for_pushes(4).await;
    assert!(
        pushed[2..].iter().all(|(_, text)| text.contains("|C|")),
        "{pushed:?}"
    );
    assert_eq!(lab.status("S123", "CREA"), TestStatus::Cancelled);
    assert_eq!(lab.status("S123", "GLU"), TestStatus::Resulted);
    let answer = records(&lab.ask("chem-1", &query("S123", "ALL")).await);
    assert_eq!(answer.last().unwrap(), "L|1|I");

    lab.engine.shutdown().await;
}

#[test]
fn step_settings_are_checked() {
    let mut registry = Registry::new();
    let environment = LabEnvironment::with_cache(Arc::new(OrderCache::open_in_memory().unwrap()));
    oxim_lab::register(&mut registry, environment);
    for transformers in [
        "[{type: select-tests, device: chem-1}]",
        "[{type: select-tests, device: chem-1, routing: missing.csv}]",
        "[{type: answer-query, routing: routing.csv}]",
        "[{type: answer-query, mark_sent: maybe}]",
        "[{type: cache-orders, require_specimen: 1}]",
    ] {
        let yaml = format!(
            "id: a\nsource: {{type: x, data_type: hl7v2, normalize: true}}\ntransformers: {transformers}\n"
        );
        let config = ChannelConfig::from_yaml(&yaml).unwrap();
        assert!(registry.compile(&config).is_err(), "{transformers}");
    }
}
