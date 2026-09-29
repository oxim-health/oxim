//! Engine channels with the FHIR normalizer and encoders (synthetic data).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use oxim_core::{
    ChannelConfig, ConnectorError, DestinationConnector, Engine, EngineOptions, Registry,
    SendError, SourceConnector, SourceContext, SubmitInfo, SystemClock, async_trait,
};
use oxim_fhir::Resource;
use oxim_model::{DataType, MessageId, MessageStatus};
use oxim_store::{Delivery, SqliteStore};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

type Submission = (Vec<u8>, oneshot::Sender<MessageId>);

/// Destination, data type and payload of a delivery.
type Sent = (String, Option<DataType>, Vec<u8>);

#[derive(Debug)]
struct TestSource {
    inbox: tokio::sync::Mutex<mpsc::Receiver<Submission>>,
}

#[async_trait]
impl SourceConnector for TestSource {
    async fn run(&self, context: SourceContext) -> Result<(), ConnectorError> {
        let mut inbox = self.inbox.lock().await;
        loop {
            tokio::select! {
                () = context.cancelled() => return Ok(()),
                next = inbox.recv() => {
                    let Some((raw, reply)) = next else { return Ok(()) };
                    let id = context.submit(raw, SubmitInfo::default()).await.map_err(|e| ConnectorError(e.to_string()))?;
                    let _ = reply.send(id);
                }
            }
        }
    }
}

/// Records deliveries per destination.
#[derive(Debug, Default)]
struct Recorder {
    sent: Mutex<Vec<Sent>>,
}

#[async_trait]
impl DestinationConnector for Recorder {
    async fn send(&self, delivery: &Delivery) -> Result<Option<Vec<u8>>, SendError> {
        self.sent.lock().unwrap().push((
            delivery.destination.to_string(),
            delivery.data_type,
            delivery.payload.clone(),
        ));
        Ok(None)
    }
}

const HL7_ORU: &[u8] =
    b"MSH|^~\\&|MIDDLEWARE|LAB|LIS|HOSP|20260929143000+0300||ORU^R01^ORU_R01|MSG1|P|2.5.1\r\
PID|1||42^^^HOSP^MR||Doe^John||19750305|M\r\
ORC|RE|ORD7|S900\r\
OBR|1|ORD7|S900|2345-7^Glucose^LN|||20260929120000\r\
SPM|1|S900^S900||SER^Serum^HL70487|||||||||||||20260929120000\r\
OBX|1|NM|2345-7^Glucose^LN||5.40|mmol/L^mmol/L^UCUM|3.9-6.1|N|||F|||20260929142500\r";

/// Starts an engine with one channel and a recorder, submits `raw` and
/// waits until the message is processed and delivered.
async fn run_channel(yaml: &str, raw: &[u8]) -> (MessageId, Vec<Sent>) {
    let (inbox, rx) = mpsc::channel(4);
    let source = Arc::new(TestSource {
        inbox: tokio::sync::Mutex::new(rx),
    });
    let recorder = Arc::new(Recorder::default());
    let mut registry = Registry::new();
    oxim_mapping::register(&mut registry);
    oxim_fhir::register(&mut registry);
    registry.add_source("test", move |_| {
        Ok(source.clone() as Arc<dyn SourceConnector>)
    });
    let target = recorder.clone();
    registry.add_destination("recorder", move |_| {
        Ok(target.clone() as Arc<dyn DestinationConnector>)
    });
    let mut options = EngineOptions::default();
    options.idle_poll = Duration::from_millis(50);
    let engine = Engine::start(
        Box::new(SqliteStore::open_in_memory().unwrap()),
        registry,
        Arc::new(SystemClock),
        options,
    )
    .await
    .unwrap();
    engine
        .deploy(ChannelConfig::from_yaml(yaml).unwrap())
        .await
        .unwrap();

    let (reply, id) = oneshot::channel();
    inbox.send((raw.to_vec(), reply)).await.unwrap();
    let id = id.await.unwrap();
    for _ in 0..400 {
        let status = engine
            .store()
            .run(move |s| s.message(id))
            .await
            .unwrap()
            .unwrap()
            .status;
        if status == MessageStatus::Completed {
            break;
        }
        assert_ne!(status, MessageStatus::Error, "processing failed");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    engine.shutdown().await;
    let mut sent = recorder.sent.lock().unwrap().clone();
    sent.sort_by(|a, b| a.0.cmp(&b.0));
    (id, sent)
}

#[tokio::test(flavor = "multi_thread")]
async fn hl7_results_are_delivered_as_fhir() {
    let (id, sent) = run_channel(
        "id: chemistry
source:
  type: test
  data_type: hl7v2
  normalize: true
destinations:
  - id: bundle
    type: recorder
    encoder:
      type: fhir-bundle
      patient_identifier_system: urn:oid:2.16.840.1.113883.19.5
      utc_offset: 180
  - id: observation
    type: recorder
    encoder:
      type: fhir-resource-json
      resource: Observation
      utc_offset: 180
",
        HL7_ORU,
    )
    .await;
    assert_eq!(sent.len(), 2);

    let (name, data_type, bundle) = &sent[0];
    assert_eq!(name, "bundle");
    assert_eq!(*data_type, Some(DataType::Fhir));
    let Resource::Bundle(bundle) = Resource::from_json(bundle).unwrap() else {
        panic!("expected a bundle");
    };
    assert_eq!(bundle.kind, "transaction");
    assert!(oxim_fhir::validate(&Resource::Bundle(bundle.clone())).is_empty());
    // Full URLs derive from the message id. The sending application has
    // no identifier, so there is no Device entry and the Patient is first.
    assert_eq!(
        bundle.entry[0].full_url.as_deref(),
        Some(oxim_fhir::entry_url(id, "patient-0").as_str())
    );
    let text = String::from_utf8(sent[0].2.clone()).unwrap();
    assert!(text.contains(r#""value":5.40"#), "{text}");
    assert!(text.contains("urn:oid:2.16.840.1.113883.19.5|42"), "{text}");

    let (name, data_type, observation) = &sent[1];
    assert_eq!(name, "observation");
    assert_eq!(*data_type, Some(DataType::Fhir));
    let json: Value = serde_json::from_slice(observation).unwrap();
    assert_eq!(json["resourceType"], "Observation");
    assert_eq!(json["valueQuantity"]["value"].to_string(), "5.40");
    // Bundle-internal references become logical references.
    assert_eq!(json["subject"]["type"], "Patient");
    assert_eq!(json["subject"]["identifier"]["value"], "42");
    assert!(!String::from_utf8_lossy(observation).contains("urn:uuid:"));
}

#[tokio::test(flavor = "multi_thread")]
async fn fhir_messages_are_normalized() {
    let bundle = br#"{
      "resourceType": "Bundle",
      "type": "collection",
      "entry": [
        {"fullUrl": "urn:uuid:5f0c2a4e-0000-4000-8000-000000000001",
         "resource": {"resourceType": "Patient", "identifier": [{"value": "PID001"}],
                      "name": [{"family": "Doe", "given": ["Jane"]}]}},
        {"fullUrl": "urn:uuid:5f0c2a4e-0000-4000-8000-000000000002",
         "resource": {"resourceType": "Observation", "status": "final",
                      "code": {"coding": [{"system": "http://loinc.org", "code": "2345-7", "display": "Glucose"}]},
                      "subject": {"reference": "urn:uuid:5f0c2a4e-0000-4000-8000-000000000001"},
                      "valueQuantity": {"value": 5.40, "unit": "mmol/L"}}}
      ]
    }"#;
    let (_, sent) = run_channel(
        "id: fhir-in
source:
  type: test
  data_type: fhir
  normalize: true
destinations:
  - id: lis
    type: recorder
    encoder:
      type: hl7v2-oru-r01
      receiving_application: LIS
",
        bundle,
    )
    .await;
    assert_eq!(sent.len(), 1);
    let oru = oxim_hl7::Message::parse(&sent[0].2).unwrap();
    assert_eq!(oru.get("PID-3.1").unwrap(), "PID001");
    assert_eq!(oru.get("OBX-3.1").unwrap(), "2345-7");
    assert_eq!(oru.get("OBX-5").unwrap(), "5.40");
}
