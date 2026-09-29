//! Shared test helpers: synthetic DICOM objects, an engine with the DICOM
//! components and a recording destination, and a scriptable Storage SCP.
//!
//! Every object is synthetic; no real patient data is used.

#![allow(
    dead_code,
    unreachable_pub,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dicom_core::value::DataSetSequence;
use dicom_core::{DataElement, PrimitiveValue, VR};
use dicom_dictionary_std::{tags, uids};
use dicom_object::InMemDicomObject;
use dicom_ul::association::server::ServerAssociationOptions;
use dicom_ul::pdu::{PDataValue, PDataValueType, Pdu};
use oxim_core::{
    ChannelConfig, DestinationConnector, Document, Engine, EngineOptions, MessageContext, Registry,
    SendError, SystemClock, async_trait,
};
use oxim_dicom::dimse::Command;
use oxim_dicom::part10::FileMeta;
use oxim_dicom::{DicomObject, DicomScu, DicomScuSettings};
use oxim_model::{ChannelId, ConnectorId, DataType, Envelope, MessageId, Timestamp};
use oxim_store::{Delivery, MessageQuery, MessageRecord, SqliteStore};
use tokio::net::TcpListener;

pub mod pacs;

pub const CT_IMAGE_STORAGE: &str = uids::CT_IMAGE_STORAGE;

/// A test PKI written as PEM files: a CA, a server certificate for
/// `localhost` and a client certificate.
pub struct Pki {
    pub dir: tempfile::TempDir,
    pub ca: std::path::PathBuf,
    pub server_cert: std::path::PathBuf,
    pub server_key: std::path::PathBuf,
    pub client_cert: std::path::PathBuf,
    pub client_key: std::path::PathBuf,
}

impl Pki {
    pub fn new() -> Self {
        use rcgen::{
            BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer,
            KeyPair, KeyUsagePurpose,
        };
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, text: String| {
            let path = dir.path().join(name);
            std::fs::write(&path, text).unwrap();
            path
        };
        let ca_key = KeyPair::generate().unwrap();
        let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params
            .distinguished_name
            .push(DnType::CommonName, "OXIM Test CA");
        ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let ca_cert = ca_params.self_signed(&ca_key).unwrap();
        let issuer = Issuer::new(ca_params, ca_key);
        let leaf = |names: Vec<String>, usage: ExtendedKeyUsagePurpose| {
            let key = KeyPair::generate().unwrap();
            let mut params = CertificateParams::new(names).unwrap();
            params.extended_key_usages = vec![usage];
            let cert = params.signed_by(&key, &issuer).unwrap();
            (cert.pem(), key.serialize_pem())
        };
        let (server_cert, server_key) = leaf(
            vec!["localhost".into()],
            ExtendedKeyUsagePurpose::ServerAuth,
        );
        let (client_cert, client_key) = leaf(
            vec!["modality.test".into()],
            ExtendedKeyUsagePurpose::ClientAuth,
        );
        Self {
            ca: write("ca.pem", ca_cert.pem()),
            server_cert: write("server.pem", server_cert),
            server_key: write("server-key.pem", server_key),
            client_cert: write("client.pem", client_cert),
            client_key: write("client-key.pem", client_key),
            dir,
        }
    }
}

/// A path for YAML, with forward slashes.
pub fn yaml_path(path: &std::path::Path) -> String {
    format!("'{}'", path.display().to_string().replace('\\', "/"))
}

/// A synthetic object of one study.
#[derive(Debug, Clone)]
pub struct Synthetic {
    pub sop_class_uid: &'static str,
    pub study_uid: String,
    pub series_uid: String,
    pub instance_uid: String,
    pub modality: &'static str,
    pub patient_id: &'static str,
    pub transfer_syntax: &'static str,
}

impl Synthetic {
    pub fn ct(instance: u32) -> Self {
        Self {
            sop_class_uid: CT_IMAGE_STORAGE,
            study_uid: "1.2.826.0.1.3680043.10.1.1".into(),
            series_uid: "1.2.826.0.1.3680043.10.1.1.1".into(),
            instance_uid: format!("1.2.826.0.1.3680043.10.1.1.1.{instance}"),
            modality: "CT",
            patient_id: "SYN-0001",
            transfer_syntax: oxim_dicom::uids::EXPLICIT_VR_LITTLE_ENDIAN,
        }
    }

    pub fn with_transfer_syntax(mut self, transfer_syntax: &'static str) -> Self {
        self.transfer_syntax = transfer_syntax;
        self
    }

    pub fn with_modality(mut self, modality: &'static str) -> Self {
        self.modality = modality;
        self
    }

    /// The data set.
    pub fn dataset(&self) -> InMemDicomObject {
        let text = |tag, vr, value: &str| DataElement::new(tag, vr, PrimitiveValue::from(value));
        let reference = InMemDicomObject::from_element_iter([
            text(tags::REFERENCED_SOP_CLASS_UID, VR::UI, CT_IMAGE_STORAGE),
            text(
                tags::REFERENCED_SOP_INSTANCE_UID,
                VR::UI,
                &format!("{}.99", self.series_uid),
            ),
        ]);
        let pixels: Vec<u16> = (0..16).collect();
        InMemDicomObject::from_element_iter([
            text(tags::SPECIFIC_CHARACTER_SET, VR::CS, "ISO_IR 192"),
            text(tags::SOP_CLASS_UID, VR::UI, self.sop_class_uid),
            text(tags::SOP_INSTANCE_UID, VR::UI, &self.instance_uid),
            text(tags::STUDY_DATE, VR::DA, "20240229"),
            text(tags::STUDY_TIME, VR::TM, "101500"),
            text(tags::ACCESSION_NUMBER, VR::SH, "ACC-7"),
            text(tags::MODALITY, VR::CS, self.modality),
            text(tags::INSTITUTION_NAME, VR::LO, "Synthetic Hospital"),
            text(tags::REFERRING_PHYSICIAN_NAME, VR::PN, "SYNTHETIC^REFERRER"),
            text(tags::STUDY_DESCRIPTION, VR::LO, "Synthetic study"),
            DataElement::new(
                tags::REFERENCED_IMAGE_SEQUENCE,
                VR::SQ,
                DataSetSequence::from(vec![reference]),
            ),
            text(tags::PATIENT_NAME, VR::PN, "SYNTHETIC^PATIENT"),
            text(tags::PATIENT_ID, VR::LO, self.patient_id),
            text(tags::PATIENT_BIRTH_DATE, VR::DA, "19800115"),
            text(tags::PATIENT_SEX, VR::CS, "O"),
            text(tags::STUDY_INSTANCE_UID, VR::UI, &self.study_uid),
            text(tags::SERIES_INSTANCE_UID, VR::UI, &self.series_uid),
            text(tags::STUDY_ID, VR::SH, "S1"),
            text(
                tags::FRAME_OF_REFERENCE_UID,
                VR::UI,
                &format!("{}.0", self.study_uid),
            ),
            DataElement::new(tags::ROWS, VR::US, PrimitiveValue::from(4u16)),
            DataElement::new(tags::COLUMNS, VR::US, PrimitiveValue::from(4u16)),
            DataElement::new(tags::BITS_ALLOCATED, VR::US, PrimitiveValue::from(16u16)),
            // A private attribute with its creator.
            text(dicom_core::Tag(0x0009, 0x0010), VR::LO, "OXIM TEST"),
            text(dicom_core::Tag(0x0009, 0x1001), VR::LO, "private detail"),
            DataElement::new(
                tags::PIXEL_DATA,
                VR::OW,
                PrimitiveValue::U16(pixels.into_iter().collect()),
            ),
        ])
    }

    /// The Part 10 object.
    pub fn bytes(&self) -> Vec<u8> {
        DicomObject {
            meta: FileMeta {
                sop_class_uid: self.sop_class_uid.to_owned(),
                sop_instance_uid: self.instance_uid.clone(),
                transfer_syntax: self.transfer_syntax.to_owned(),
                source_ae_title: Some("MODALITY".into()),
                sending_ae_title: None,
                receiving_ae_title: None,
            },
            dataset: self.dataset(),
        }
        .to_bytes()
        .unwrap()
    }
}

/// The text of a top-level attribute.
pub fn text(object: &DicomObject, name: &str) -> Option<String> {
    object.text(&oxim_dicom::parse_selector(name).unwrap())
}

/// A destination that records every payload.
#[derive(Debug, Default)]
pub struct Recorder {
    pub sent: Mutex<Vec<Vec<u8>>>,
}

impl Recorder {
    pub fn payloads(&self) -> Vec<Vec<u8>> {
        self.sent.lock().unwrap().clone()
    }
}

#[async_trait]
impl DestinationConnector for Recorder {
    async fn send(&self, delivery: &Delivery) -> Result<Option<Vec<u8>>, SendError> {
        self.sent.lock().unwrap().push(delivery.payload.clone());
        Ok(None)
    }
}

/// An engine with the DICOM components plus a `recorder` destination.
pub async fn engine(recorder: Arc<Recorder>) -> Engine {
    engine_with(recorder, |_| {}).await
}

/// An engine with the DICOM components, a `recorder` destination and what
/// `extra` registers.
pub async fn engine_with(recorder: Arc<Recorder>, extra: impl FnOnce(&mut Registry)) -> Engine {
    let mut registry = Registry::new();
    oxim_dicom::register(&mut registry);
    extra(&mut registry);
    registry.add_destination("recorder", move |_| {
        Ok(recorder.clone() as Arc<dyn DestinationConnector>)
    });
    let mut options = EngineOptions::default();
    options.idle_poll = Duration::from_millis(50);
    options.shutdown_grace = Duration::from_secs(5);
    options.source_restart_delay = Duration::from_millis(100);
    Engine::start(
        Box::new(SqliteStore::open_in_memory().unwrap()),
        registry,
        Arc::new(SystemClock),
        options,
    )
    .await
    .unwrap()
}

/// Deploys a channel from YAML.
pub async fn deploy(engine: &Engine, yaml: &str) {
    engine
        .deploy(ChannelConfig::from_yaml(yaml).unwrap())
        .await
        .unwrap();
}

/// Every stored message, newest first.
pub async fn messages(engine: &Engine) -> Vec<MessageRecord> {
    engine
        .store()
        .run(|store| store.list_messages(&MessageQuery::default()))
        .await
        .unwrap()
}

/// A local port that was free a moment ago.
pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Polls `condition` every 10 ms for up to 10 s.
pub async fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    for _ in 0..1000 {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting until {what}");
}

/// An SCU for `port`.
pub fn scu(port: u16, calling: &str, called: &str) -> DicomScu {
    DicomScu::new(DicomScuSettings {
        target: format!("127.0.0.1:{port}"),
        called_ae_title: called.to_owned(),
        calling_ae_title: calling.to_owned(),
        connect_timeout: oxim_core::config::DurationText(Duration::from_secs(5)),
        timeout: oxim_core::config::DurationText(Duration::from_secs(10)),
        max_pdu_length: 16_384,
        transcode: true,
        storage_commitment: None,
        tls: None,
    })
    .unwrap()
}

/// Stores `object` through `scu`, retrying while the SCP starts listening.
pub async fn store_when_ready(scu: &DicomScu, object: &[u8]) -> oxim_dicom::StoreResponse {
    for _ in 0..500 {
        match scu.store(object).await {
            Ok(response) => return response,
            Err(e) if e.message.contains("refused") || e.message.contains("failed") => {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(e) => panic!("store failed: {e}"),
        }
    }
    panic!("the SCP never answered");
}

/// A delivery of `payload` for direct destination tests.
pub fn delivery(n: u64, payload: &[u8]) -> Delivery {
    Delivery {
        message_id: MessageId::from_parts(1_790_000_000_000 + n, u128::from(n)),
        channel: ChannelId::new("imaging").unwrap(),
        destination: ConnectorId::new("out").unwrap(),
        attempts: 0,
        payload: payload.to_vec(),
        data_type: Some(DataType::Dicom),
    }
}

/// A pipeline context holding a DICOM message.
pub fn context(bytes: Vec<u8>) -> MessageContext {
    let envelope = Envelope::new(
        MessageId::from_parts(1_790_000_000_000, 1),
        ChannelId::new("imaging").unwrap(),
        ConnectorId::new("source").unwrap(),
        Timestamp::from_unix_millis(1_790_000_000_000).unwrap(),
        DataType::Dicom,
        bytes.clone(),
    );
    MessageContext {
        envelope,
        document: Document::Raw(bytes),
        clinical: None,
        variables: BTreeMap::new(),
        response: None,
    }
}

/// The raw bytes of a context's document.
pub fn raw(context: &MessageContext) -> Vec<u8> {
    context.document.to_bytes()
}

/// What the scripted SCP received.
#[derive(Debug, Default)]
pub struct Received {
    pub commands: Vec<Command>,
    pub data_sets: Vec<Vec<u8>>,
    pub transfer_syntaxes: Vec<String>,
}

/// A Storage SCP that answers each C-STORE with the next status of
/// `statuses`, one association per request. It accepts `sop_class` in
/// `transfer_syntaxes` (any supported syntax when empty).
pub async fn scripted_scp(
    sop_class: &'static str,
    transfer_syntaxes: &'static [&'static str],
    statuses: Vec<u16>,
) -> (u16, Arc<Mutex<Received>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let received = Arc::new(Mutex::new(Received::default()));
    let log = received.clone();
    tokio::spawn(async move {
        let mut options = ServerAssociationOptions::new()
            .accept_any()
            .ae_title("SCRIPTED")
            .with_abstract_syntax(sop_class);
        for ts in transfer_syntaxes {
            options = options.with_transfer_syntax(*ts);
        }
        'next: for status in statuses {
            let (stream, _) = listener.accept().await.unwrap();
            let Ok(mut association) = options.establish_async(stream).await else {
                continue;
            };
            let contexts = association.presentation_contexts().to_vec();
            let mut command = Vec::new();
            let mut data = Vec::new();
            let mut context_id = 0;
            loop {
                match association.receive().await {
                    Ok(Pdu::PData { data: values }) => {
                        let mut done = false;
                        for value in values {
                            context_id = value.presentation_context_id;
                            match value.value_type {
                                PDataValueType::Command => command.extend(value.data),
                                PDataValueType::Data => {
                                    data.extend(value.data);
                                    done = value.is_last;
                                }
                            }
                        }
                        if done {
                            break;
                        }
                    }
                    // The SCU gave up, for example without a usable
                    // presentation context.
                    _ => continue 'next,
                }
            }
            let request = Command::decode(&command).unwrap();
            let response = Command::response_to(&request, status, Some("scripted"));
            let ts = contexts
                .iter()
                .find(|pc| pc.id == context_id)
                .map(|pc| pc.transfer_syntax.clone())
                .unwrap();
            {
                let mut log = log.lock().unwrap();
                log.commands.push(request);
                log.data_sets.push(data);
                log.transfer_syntaxes.push(ts);
            }
            association
                .send(&Pdu::PData {
                    data: vec![PDataValue {
                        presentation_context_id: context_id,
                        value_type: PDataValueType::Command,
                        is_last: true,
                        data: response.encode(),
                    }],
                })
                .await
                .unwrap();
            if let Ok(Pdu::ReleaseRQ) = association.receive().await {
                let _ = association.send(&Pdu::ReleaseRP).await;
            }
        }
    });
    (port, received)
}
