//! A scriptable archive for tests: C-MOVE and C-GET of the objects it
//! holds, C-STORE, and storage commitment with configurable reports.
//!
//! Every object is synthetic; no real patient data is used.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dicom_core::value::DataSetSequence;
use dicom_core::{DataElement, PrimitiveValue, VR};
use dicom_dictionary_std::tags;
use dicom_object::InMemDicomObject;
use dicom_transfer_syntax_registry::{TransferSyntaxIndex, TransferSyntaxRegistry};
use dicom_ul::association::client::ClientAssociationOptions;
use dicom_ul::association::server::{Negotiation, ServerAssociationOptions};
use dicom_ul::pdu::{PDataValue, PDataValueType, Pdu, RequestorRoles};
use oxim_dicom::dimse::{Command, command_field, element, status};
use oxim_dicom::part10;
use oxim_dicom::uids;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;

/// Accepts every role a requester asks for.
#[derive(Debug, Clone, Copy)]
pub struct AnyRoles;

impl Negotiation for AnyRoles {
    fn negotiate_roles(
        &self,
        _sop_class_uid: &str,
        scu: bool,
        scp: bool,
    ) -> Option<RequestorRoles> {
        Some(RequestorRoles { scu, scp })
    }
}

/// A command and its data set.
#[derive(Debug, Clone)]
pub struct Received {
    pub context_id: u8,
    pub command: Command,
    pub data: Vec<u8>,
}

/// Something that sends and receives PDUs.
pub trait Pdus {
    fn send_pdu(&mut self, pdu: &Pdu) -> impl Future<Output = bool> + Send;
    fn receive_pdu(&mut self) -> impl Future<Output = Option<Pdu>> + Send;
}

impl<S> Pdus for dicom_ul::association::AsyncServerAssociation<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    async fn send_pdu(&mut self, pdu: &Pdu) -> bool {
        self.send(pdu).await.is_ok()
    }

    async fn receive_pdu(&mut self) -> Option<Pdu> {
        self.receive().await.ok()
    }
}

impl<S> Pdus for dicom_ul::association::AsyncClientAssociation<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    async fn send_pdu(&mut self, pdu: &Pdu) -> bool {
        self.send(pdu).await.is_ok()
    }

    async fn receive_pdu(&mut self) -> Option<Pdu> {
        self.receive().await.ok()
    }
}

/// Receives the next message; `None` when the association ends.
pub async fn receive(link: &mut impl Pdus) -> Option<Received> {
    let mut command = Vec::new();
    let mut data = Vec::new();
    let mut decoded: Option<Command> = None;
    loop {
        match link.receive_pdu().await? {
            Pdu::PData { data: values } => {
                for value in values {
                    let context_id = value.presentation_context_id;
                    match value.value_type {
                        PDataValueType::Command => {
                            command.extend(value.data);
                            if value.is_last {
                                let parsed = Command::decode(&command).ok()?;
                                if !parsed.has_data_set() {
                                    return Some(Received {
                                        context_id,
                                        command: parsed,
                                        data,
                                    });
                                }
                                decoded = Some(parsed);
                            }
                        }
                        PDataValueType::Data => {
                            data.extend(value.data);
                            if value.is_last {
                                return Some(Received {
                                    context_id,
                                    command: decoded.take()?,
                                    data,
                                });
                            }
                        }
                    }
                }
            }
            Pdu::ReleaseRQ => {
                let _ = link.send_pdu(&Pdu::ReleaseRP).await;
                return None;
            }
            _ => return None,
        }
    }
}

/// Sends a command and its data set.
pub async fn send(
    link: &mut impl Pdus,
    context_id: u8,
    command: &Command,
    data: Option<&[u8]>,
) -> bool {
    let mut values = vec![PDataValue {
        presentation_context_id: context_id,
        value_type: PDataValueType::Command,
        is_last: true,
        data: command.encode(),
    }];
    if let Some(data) = data {
        values.push(PDataValue {
            presentation_context_id: context_id,
            value_type: PDataValueType::Data,
            is_last: true,
            data: data.to_vec(),
        });
    }
    for value in values {
        if !link.send_pdu(&Pdu::PData { data: vec![value] }).await {
            return false;
        }
    }
    true
}

/// Encodes a data set in Explicit VR Little Endian.
pub fn encode(dataset: &InMemDicomObject) -> Vec<u8> {
    let ts = TransferSyntaxRegistry
        .get(uids::EXPLICIT_VR_LITTLE_ENDIAN)
        .unwrap();
    let mut out = Vec::new();
    dataset.write_dataset_with_ts(&mut out, ts).unwrap();
    out
}

/// Decodes a data set in `transfer_syntax`.
pub fn decode(data: &[u8], transfer_syntax: &str) -> InMemDicomObject {
    let ts = TransferSyntaxRegistry
        .get(transfer_syntax.trim_end_matches('\0'))
        .unwrap();
    InMemDicomObject::read_dataset_with_ts(data, ts).unwrap()
}

/// How the archive answers storage commitment requests.
#[derive(Debug, Clone)]
pub enum Commitment {
    /// Reports on the same association, committing every instance.
    Commit,
    /// Reports on the same association, failing every instance with this
    /// reason.
    Fail(u16),
    /// Never reports.
    Silent,
    /// Releases, then reports on a new association to the given port and
    /// AE title.
    Later { port: u16, ae_title: String },
}

/// What the archive does and what it saw.
#[derive(Debug)]
pub struct Pacs {
    /// The Part 10 objects it holds.
    pub objects: Vec<Vec<u8>>,
    /// Move destinations by AE title.
    pub destinations: HashMap<String, u16>,
    /// Storage commitment behavior.
    pub commitment: Commitment,
    /// Commands received, in order.
    pub log: Mutex<Vec<Command>>,
}

fn report(
    transaction: &str,
    instances: &[(String, String)],
    failure: Option<u16>,
) -> InMemDicomObject {
    let text = |tag, vr, value: &str| DataElement::new(tag, vr, PrimitiveValue::from(value));
    let items: Vec<InMemDicomObject> = instances
        .iter()
        .map(|(class, instance)| {
            let mut item = InMemDicomObject::from_element_iter([
                text(tags::REFERENCED_SOP_CLASS_UID, VR::UI, class),
                text(tags::REFERENCED_SOP_INSTANCE_UID, VR::UI, instance),
            ]);
            if let Some(reason) = failure {
                item.put(DataElement::new(
                    tags::FAILURE_REASON,
                    VR::US,
                    PrimitiveValue::from(reason),
                ));
            }
            item
        })
        .collect();
    let sequence_tag = if failure.is_some() {
        tags::FAILED_SOP_SEQUENCE
    } else {
        tags::REFERENCED_SOP_SEQUENCE
    };
    InMemDicomObject::from_element_iter([
        text(tags::TRANSACTION_UID, VR::UI, transaction),
        DataElement::new(sequence_tag, VR::SQ, DataSetSequence::from(items)),
    ])
}

fn instances_of(request: &InMemDicomObject) -> Vec<(String, String)> {
    let text = |item: &InMemDicomObject, tag| {
        item.get(tag)
            .and_then(|element| element.to_str().ok())
            .map(|value| value.trim_end_matches(['\0', ' ']).to_owned())
            .unwrap_or_default()
    };
    request
        .get(tags::REFERENCED_SOP_SEQUENCE)
        .and_then(|element| element.value().items())
        .unwrap_or_default()
        .iter()
        .map(|item| {
            (
                text(item, tags::REFERENCED_SOP_CLASS_UID),
                text(item, tags::REFERENCED_SOP_INSTANCE_UID),
            )
        })
        .collect()
}

fn transaction_of(request: &InMemDicomObject) -> String {
    request
        .get(tags::TRANSACTION_UID)
        .and_then(|element| element.to_str().ok())
        .map(|value| value.trim_end_matches(['\0', ' ']).to_owned())
        .unwrap_or_default()
}

/// Sends a commitment report on a new association.
async fn report_later(port: u16, ae_title: &str, report: &InMemDicomObject) {
    let mut association = ClientAssociationOptions::new()
        .calling_ae_title("PACS")
        .called_ae_title(ae_title.to_owned())
        .with_presentation_context(
            uids::STORAGE_COMMITMENT_PUSH,
            vec![uids::EXPLICIT_VR_LITTLE_ENDIAN],
        )
        .with_role_selection(uids::STORAGE_COMMITMENT_PUSH, false, true)
        .establish_async(("127.0.0.1", port))
        .await
        .unwrap();
    let context_id = association.presentation_contexts()[0].id;
    let command = Command::n_event_report_rq(
        7,
        uids::STORAGE_COMMITMENT_PUSH,
        uids::STORAGE_COMMITMENT_INSTANCE,
        1,
    );
    assert!(
        send(
            &mut association,
            context_id,
            &command,
            Some(&encode(report))
        )
        .await
    );
    let response = receive(&mut association).await.unwrap();
    assert_eq!(response.command.status(), Some(status::SUCCESS));
    let _ = association.release().await;
}

impl Pacs {
    /// An archive holding `objects`.
    pub fn new(objects: Vec<Vec<u8>>) -> Self {
        Self {
            objects,
            destinations: HashMap::new(),
            commitment: Commitment::Commit,
            log: Mutex::new(Vec::new()),
        }
    }

    /// Starts serving on a local port.
    pub async fn start(self) -> (u16, Arc<Self>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let pacs = Arc::new(self);
        let serving = pacs.clone();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let pacs = serving.clone();
                tokio::spawn(async move { pacs.association(stream).await });
            }
        });
        (port, pacs)
    }

    async fn association(&self, stream: tokio::net::TcpStream) {
        let options = ServerAssociationOptions::new()
            .accept_any()
            .with_negotiation(AnyRoles)
            .ae_title("PACS")
            .with_abstract_syntax(uids::VERIFICATION)
            .with_abstract_syntax(uids::STUDY_ROOT_MOVE)
            .with_abstract_syntax(uids::STUDY_ROOT_GET)
            .with_abstract_syntax(uids::STORAGE_COMMITMENT_PUSH)
            .with_abstract_syntax(uids::STUDY_ROOT_FIND)
            .with_abstract_syntax(dicom_dictionary_std::uids::CT_IMAGE_STORAGE)
            .with_transfer_syntax(uids::EXPLICIT_VR_LITTLE_ENDIAN);
        let Ok(mut association) = options.establish_async(stream).await else {
            return;
        };
        let contexts = association.presentation_contexts().to_vec();
        let mut later = None;
        let mut next_id = 100u16;
        while let Some(message) = receive(&mut association).await {
            self.log.lock().unwrap().push(message.command.clone());
            let command = &message.command;
            match command.command_field() {
                Some(command_field::C_STORE_RQ) => {
                    let response = Command::response_to(command, status::SUCCESS, None);
                    send(&mut association, message.context_id, &response, None).await;
                }
                Some(command_field::N_ACTION_RQ) => {
                    let request = decode(&message.data, uids::EXPLICIT_VR_LITTLE_ENDIAN);
                    let transaction = transaction_of(&request);
                    let instances = instances_of(&request);
                    let response = Command::response_to(command, status::SUCCESS, None);
                    send(&mut association, message.context_id, &response, None).await;
                    let event = |failure: Option<u16>| {
                        let event_type = if failure.is_some() { 2 } else { 1 };
                        (
                            Command::n_event_report_rq(
                                9,
                                uids::STORAGE_COMMITMENT_PUSH,
                                uids::STORAGE_COMMITMENT_INSTANCE,
                                event_type,
                            ),
                            encode(&report(&transaction, &instances, failure)),
                        )
                    };
                    match &self.commitment {
                        Commitment::Commit | Commitment::Fail(_) => {
                            let failure = match self.commitment {
                                Commitment::Fail(reason) => Some(reason),
                                _ => None,
                            };
                            let (command, data) = event(failure);
                            send(&mut association, message.context_id, &command, Some(&data)).await;
                        }
                        Commitment::Silent => {}
                        Commitment::Later { port, ae_title } => {
                            later = Some((
                                *port,
                                ae_title.clone(),
                                report(&transaction, &instances, None),
                            ));
                        }
                    }
                }
                Some(command_field::C_MOVE_RQ) => {
                    let destination = command.text(element::MOVE_DESTINATION).unwrap_or_default();
                    let Some(port) = self.destinations.get(&destination).copied() else {
                        let response =
                            Command::response_to(command, status::MOVE_DESTINATION_UNKNOWN, None);
                        send(&mut association, message.context_id, &response, None).await;
                        continue;
                    };
                    let scu = super::scu(port, "PACS", &destination);
                    let total = u16::try_from(self.objects.len()).unwrap();
                    let (mut completed, mut failed) = (0u16, 0u16);
                    for object in &self.objects {
                        match scu.store(object).await {
                            Ok(response) if response.status == 0 => completed += 1,
                            _ => failed += 1,
                        }
                        let pending = Command::response_to(command, status::PENDING, None)
                            .with_suboperations(total - completed - failed, completed, failed, 0);
                        send(&mut association, message.context_id, &pending, None).await;
                    }
                    let final_status = if failed == 0 {
                        status::SUCCESS
                    } else {
                        status::SUBOPERATIONS_WITH_FAILURES
                    };
                    let done = Command::response_to(command, final_status, None)
                        .with_suboperations(0, completed, failed, 0);
                    send(&mut association, message.context_id, &done, None).await;
                }
                Some(command_field::C_GET_RQ) => {
                    let total = u16::try_from(self.objects.len()).unwrap();
                    let (mut completed, mut failed) = (0u16, 0u16);
                    for object in &self.objects {
                        let parsed = part10::parse(object).unwrap();
                        let Some(storage) = contexts.iter().find(|pc| {
                            pc.abstract_syntax.trim_end_matches('\0') == parsed.meta.sop_class_uid
                                && pc.reason
                                    == dicom_ul::pdu::PresentationContextResultReason::Acceptance
                        }) else {
                            failed += 1;
                            continue;
                        };
                        next_id += 1;
                        let store = Command::c_store_rq(
                            next_id,
                            &parsed.meta.sop_class_uid,
                            &parsed.meta.sop_instance_uid,
                        );
                        send(&mut association, storage.id, &store, Some(parsed.dataset)).await;
                        let answer = receive(&mut association).await.unwrap();
                        if answer.command.status() == Some(status::SUCCESS) {
                            completed += 1;
                        } else {
                            failed += 1;
                        }
                        let pending = Command::response_to(command, status::PENDING, None)
                            .with_suboperations(total - completed - failed, completed, failed, 0);
                        send(&mut association, message.context_id, &pending, None).await;
                    }
                    let final_status = if failed == 0 {
                        status::SUCCESS
                    } else {
                        status::SUBOPERATIONS_WITH_FAILURES
                    };
                    let done = Command::response_to(command, final_status, None)
                        .with_suboperations(0, completed, failed, 0);
                    send(&mut association, message.context_id, &done, None).await;
                }
                Some(command_field::C_ECHO_RQ) => {
                    let response = Command::response_to(command, status::SUCCESS, None);
                    send(&mut association, message.context_id, &response, None).await;
                }
                _ => {}
            }
        }
        if let Some((port, ae_title, report)) = later {
            tokio::time::sleep(Duration::from_millis(50)).await;
            report_later(port, &ae_title, &report).await;
        }
    }
}
