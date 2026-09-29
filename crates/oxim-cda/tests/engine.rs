//! Engine channels with the CDA normalizer and encoder (synthetic data).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use oxim_core::{
    ChannelConfig, ConnectorError, DestinationConnector, Engine, EngineOptions, Registry,
    SendError, SourceConnector, SourceContext, SubmitInfo, SystemClock, async_trait,
};
use oxim_formats::{XmlDocument, XmlOptions};
use oxim_model::{ClinicalContent, DataType, MessageStatus};
use oxim_store::{Delivery, SqliteStore};
use tokio::sync::{mpsc, oneshot};

type Submission = (Vec<u8>, oneshot::Sender<oxim_model::MessageId>);

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

#[derive(Debug, Default)]
struct Recorder {
    sent: Mutex<Vec<(Option<DataType>, Vec<u8>)>>,
}

#[async_trait]
impl DestinationConnector for Recorder {
    async fn send(&self, delivery: &Delivery) -> Result<Option<Vec<u8>>, SendError> {
        self.sent
            .lock()
            .unwrap()
            .push((delivery.data_type, delivery.payload.clone()));
        Ok(None)
    }
}

const HL7_ORU: &[u8] =
    b"MSH|^~\\&|ANALYZER|LAB|LIS|HOSP|20260929143000+0300||ORU^R01^ORU_R01|MSG1|P|2.5.1\r\
PID|1||SYN42^^^HOSP^MR||Doe^John||19750305|M\r\
ORC|RE|ORD7|S900\r\
OBR|1|ORD7|S900|2345-7^Glucose^LN|||20260929120000\r\
SPM|1|S900^S900||SER^Serum^HL70487\r\
OBX|1|NM|2345-7^Glucose^LN||5.40|mmol/L^mmol/L^UCUM|3.9-6.1|N|||F|||20260929142500\r";

async fn run_channel(yaml: &str, raw: &[u8]) -> Vec<(Option<DataType>, Vec<u8>)> {
    let (inbox, rx) = mpsc::channel(4);
    let source = Arc::new(TestSource {
        inbox: tokio::sync::Mutex::new(rx),
    });
    let recorder = Arc::new(Recorder::default());
    let mut registry = Registry::new();
    oxim_mapping::register(&mut registry);
    oxim_cda::register(&mut registry);
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
        let record = engine
            .store()
            .run(move |s| s.message(id))
            .await
            .unwrap()
            .unwrap();
        if record.status == MessageStatus::Completed {
            break;
        }
        assert_ne!(record.status, MessageStatus::Error, "{:?}", record.error);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    engine.shutdown().await;
    recorder.sent.lock().unwrap().clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn hl7_results_become_cda_and_back() {
    let sent = run_channel(
        "id: to-cda
source:
  type: test
  data_type: hl7v2
  normalize: true
destinations:
  - id: repository
    type: recorder
    encoder:
      type: cda-lab-report
      custodian_name: Synthetic Laboratory
      custodian_id_root: 2.999.7.4
      utc_offset: 180
",
        HL7_ORU,
    )
    .await;
    assert_eq!(sent.len(), 1);
    let (data_type, cda) = &sent[0];
    assert_eq!(*data_type, Some(DataType::Cda));
    let text = String::from_utf8(cda.clone()).unwrap();
    assert!(
        text.contains(r#"<value xsi:type="PQ" value="5.40" unit="mmol/L"/>"#),
        "{text}"
    );

    // A CDA source maps the report back to results for the LIS.
    let sent = run_channel(
        "id: from-cda
source:
  type: test
  data_type: cda
  normalize: true
destinations:
  - id: lis
    type: recorder
    encoder: {type: hl7v2-oru-r01, receiving_application: LIS}
",
        cda,
    )
    .await;
    let oru = String::from_utf8(sent[0].1.clone()).unwrap();
    assert!(oru.contains("|5.40|mmol/L"), "{oru}");
    assert!(oru.contains("PID|1||SYN42"), "{oru}");

    let document = XmlDocument::parse(cda, &XmlOptions::default()).unwrap();
    let ClinicalContent::Results { groups, .. } = oxim_cda::lab_results(&document).unwrap() else {
        panic!("expected results");
    };
    assert_eq!(
        groups[0].specimen.as_ref().unwrap().identifiers[0].value,
        "S900"
    );
}

#[test]
fn settings_are_checked() {
    let mut registry = Registry::new();
    oxim_cda::register(&mut registry);
    let valid = "id: a\nsource: {type: x, data_type: cda, normalize: true}\ndestinations:\n  - {id: d, type: y, encoder: {type: cda-lab-report, custodian_id_root: 2.999.1, utc_offset: 180}}\n";
    registry
        .compile(&ChannelConfig::from_yaml(valid).unwrap())
        .unwrap();
    for encoder in [
        "{type: cda-lab-report, custodian_id_root: not-an-oid}",
        "{type: cda-lab-report, utc_offset: 5000}",
        "{type: cda-lab-report, title: 7}",
    ] {
        let yaml = format!(
            "id: a\nsource: {{type: x, data_type: cda, normalize: true}}\ndestinations:\n  - {{id: d, type: y, encoder: {encoder}}}\n"
        );
        let config = ChannelConfig::from_yaml(&yaml).unwrap();
        assert!(registry.compile(&config).is_err(), "{encoder}");
    }
}
