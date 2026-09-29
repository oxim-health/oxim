//! A host query for a tube the order cache does not know is passed to the
//! LIS; its answer is cached and returned to the analyzer.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use oxim_core::{
    ChannelConfig, DestinationConnector, Engine, EngineOptions, Registry, Reply, SendError,
    SourceConnector, SourceContext, SubmitInfo, SystemClock, async_trait,
};
use oxim_lab::{LabEnvironment, OrderCache, TestStatus};
use oxim_store::{Delivery, SqliteStore};
use tokio::sync::{Mutex, mpsc, oneshot};

type Request = (Vec<u8>, oneshot::Sender<Reply>);

/// The analyzer's connection: each request waits for its reply.
#[derive(Debug)]
struct Analyzer {
    inbox: Mutex<mpsc::Receiver<Request>>,
}

#[async_trait]
impl SourceConnector for Analyzer {
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

/// A LIS that answers work order queries for tube S777 with a glucose
/// order, and "not found" for any other tube.
#[derive(Debug, Default)]
struct Lis {
    queries: StdMutex<Vec<String>>,
}

#[async_trait]
impl DestinationConnector for Lis {
    async fn send(&self, delivery: &Delivery) -> Result<Option<Vec<u8>>, SendError> {
        let query = String::from_utf8(delivery.payload.clone()).unwrap();
        self.queries.lock().unwrap().push(query.clone());
        let tag = query
            .split('\r')
            .find(|segment| segment.starts_with("QPD|"))
            .and_then(|qpd| qpd.split('|').nth(2))
            .unwrap_or_default()
            .to_owned();
        let found = query.contains("|S777");
        let mut answer = format!(
            "MSH|^~\\&|LIS|LAB|OXIM|LAB|20260929120000||RSP^K11^RSP_K11|A1|P|2.5.1\r\
MSA|AA|Q1\rQAK|{tag}|{}|WOS^Work Order Step^IHE_LABTF\r\
QPD|WOS^Work Order Step^IHE_LABTF|{tag}|S777\r",
            if found { "OK" } else { "NF" }
        );
        if found {
            answer.push_str(
                "PID|1||P2002^^^LAB^MR||ROE^RICHARD\rSPM|1|S777||SER\r\
ORC|NW|ORD7\rOBR|1|ORD7||GLU^Glucose^L\r",
            );
        }
        Ok(Some(answer.into_bytes()))
    }
}

fn query(specimen: &str) -> Vec<u8> {
    format!("H|\\^&|||CHEM^1.0\rQ|1|^{specimen}||ALL||||||||O\rL|1|N\r").into_bytes()
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_tubes_are_asked_of_the_lis() {
    let (tx, rx) = mpsc::channel(8);
    let analyzer = Arc::new(Analyzer {
        inbox: Mutex::new(rx),
    });
    let lis = Arc::new(Lis::default());
    let cache = Arc::new(OrderCache::open_in_memory().unwrap());
    let mut registry = Registry::new();
    oxim_mapping::register(&mut registry);
    oxim_lab::register(&mut registry, LabEnvironment::with_cache(cache.clone()));
    let lis_connector = lis.clone();
    registry
        .add_source("analyzer", move |_| {
            Ok(analyzer.clone() as Arc<dyn SourceConnector>)
        })
        .add_destination("lis", move |_| {
            Ok(lis_connector.clone() as Arc<dyn DestinationConnector>)
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
    engine
        .deploy(
            ChannelConfig::from_yaml(
                "id: chem-1
source:
  type: analyzer
  data_type: astm
  normalize: true
  response:
    mode: pipeline
    timeout: 5s
    encoder: {type: astm-query-response, sender: OXIM}
    fallback:
      destination: lis
      data_type: hl7v2
      transformers:
        - {type: cache-orders, mark_sent: true, device: chem-1}
transformers:
  - {type: answer-query, on_missing: ask}
destinations:
  - id: lis
    type: lis
    filters: [{type: clinical-kind, kinds: [query]}]
    encoder: {type: hl7v2-qbp-q11, sending_application: OXIM, receiving_application: LIS}
    queue: {retry: {initial_delay: 100ms, max_delay: 100ms}}
",
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let ask = |raw: Vec<u8>| {
        let tx = tx.clone();
        async move {
            let (reply, answer) = oneshot::channel();
            tx.send((raw, reply)).await.unwrap();
            answer.await.unwrap()
        }
    };

    // The cache does not know S777: the LIS is asked with QBP^Q11 and its
    // answer reaches the analyzer as ASTM.
    let reply = ask(query("S777")).await;
    let text = String::from_utf8(reply.data.clone().expect("a reply")).unwrap();
    assert!(text.contains("\rO|1|S777||^^^GLU|"), "{text}");
    assert!(text.ends_with("L|1|N\r"), "{text}");
    let sent = lis.queries.lock().unwrap().clone();
    assert_eq!(sent.len(), 1);
    assert!(sent[0].contains("QBP^Q11^QBP_Q11"), "{}", sent[0]);
    assert!(
        sent[0].contains("\rQPD|WOS^Work Order Step^IHE_LABTF|"),
        "{}",
        sent[0]
    );
    // The LIS's order is cached and was offered to the analyzer.
    let order = cache.get("S777").unwrap().expect("cached");
    assert_eq!(order.tests[0].status, TestStatus::Sent);
    assert_eq!(order.tests[0].device.as_deref(), Some("chem-1"));

    // Now the cache knows the tube: the LIS is not asked again.
    let reply = ask(query("S777")).await;
    let text = String::from_utf8(reply.data.unwrap()).unwrap();
    assert!(text.contains("\rO|1|S777||^^^GLU|"), "{text}");
    assert_eq!(lis.queries.lock().unwrap().len(), 1);

    // A tube the LIS does not know either gets "no information".
    let reply = ask(query("S404")).await;
    let text = String::from_utf8(reply.data.unwrap()).unwrap();
    assert!(text.ends_with("L|1|I\r"), "{text}");
    assert_eq!(lis.queries.lock().unwrap().len(), 2);
    engine.shutdown().await;
}
