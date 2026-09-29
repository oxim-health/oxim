//! Typed views of common POCT1-A message content.
//!
//! The accessors use element names that appear in published POCT1-A
//! examples and vendor interface documents (`HDR.control_id`,
//! `DEV.device_id`, `DST.new_observations_qty`, `SVC.role_cd`,
//! `PT.patient_id`, `OBS.observation_id`, `OBS.value`, `OPR.operator_id`,
//! ...). Vendors add their own elements; everything remains reachable
//! through [`Element`] and [`Message::find`], and device profiles map vendor
//! specifics on top of these views.

use crate::element::Element;
use crate::message::Message;

/// What an observation measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ObservationKind {
    /// A patient result (`SVC.role_cd` `OBS`, or an observation under `PT`).
    Patient,
    /// A quality control result (`SVC.role_cd` `LQC`, `EQC` or `QC`, or an
    /// observation under a control `CTC`).
    QualityControl,
    /// A calibration result (`SVC.role_cd` `CAL`).
    Calibration,
    /// A proficiency testing result (`SVC.role_cd` `PRF`).
    Proficiency,
    /// A role the crate does not recognize.
    Other,
}

/// One observation (`OBS` element) of an observation message, together
/// with the service event (`SVC`) and subject (`PT` or `CTC`) it belongs to.
#[derive(Debug, Clone, Copy)]
pub struct Observation<'a> {
    element: &'a Element,
    service: Option<&'a Element>,
    subject: Option<&'a Element>,
}

impl<'a> Observation<'a> {
    /// The `OBS` element.
    pub fn element(&self) -> &'a Element {
        self.element
    }

    /// The enclosing service event (`SVC`), if any.
    pub fn service(&self) -> Option<&'a Element> {
        self.service
    }

    /// The enclosing subject: a patient (`PT`) or control material (`CTC`).
    pub fn subject(&self) -> Option<&'a Element> {
        self.subject
    }

    /// The `V` value of a direct child of the `OBS` element, for example
    /// `field("OBS.method_cd")`.
    pub fn field(&self, name: &str) -> Option<&'a str> {
        self.element.first(name).and_then(Element::value)
    }

    /// `SVC.role_cd`.
    pub fn role_code(&self) -> Option<&'a str> {
        self.service.and_then(|s| s.value_at("SVC.role_cd"))
    }

    /// What the observation measured.
    pub fn kind(&self) -> ObservationKind {
        match self.role_code() {
            Some("OBS") => ObservationKind::Patient,
            Some("LQC" | "EQC" | "QC") => ObservationKind::QualityControl,
            Some("CAL") => ObservationKind::Calibration,
            Some("PRF") => ObservationKind::Proficiency,
            Some(_) => ObservationKind::Other,
            None => match self.subject.map(Element::name) {
                Some("PT") => ObservationKind::Patient,
                Some("CTC") => ObservationKind::QualityControl,
                _ => ObservationKind::Other,
            },
        }
    }

    /// `OBS.observation_id`, the analyte or test code.
    pub fn observation_id(&self) -> Option<&'a str> {
        self.field("OBS.observation_id")
    }

    /// The coding system of the observation ID (`SN` attribute).
    pub fn coding_system(&self) -> Option<&'a str> {
        self.element
            .first("OBS.observation_id")
            .and_then(|e| e.attribute_value("SN"))
    }

    /// `OBS.value`, the result as sent by the device.
    pub fn value(&self) -> Option<&'a str> {
        self.field("OBS.value")
    }

    /// The unit of the value (`U` attribute of `OBS.value`).
    pub fn unit(&self) -> Option<&'a str> {
        self.element
            .first("OBS.value")
            .and_then(|e| e.attribute_value("U"))
    }

    /// `OBS.method_cd`.
    pub fn method(&self) -> Option<&'a str> {
        self.field("OBS.method_cd")
    }

    /// The device's interpretation or normal flag, from the first present
    /// of `OBS.interpretation_cd`, `OBS.normal_flag` and `OBS.qualifier_cd`.
    /// OXIM passes it on unchanged and never computes one.
    pub fn interpretation(&self) -> Option<&'a str> {
        [
            "OBS.interpretation_cd",
            "OBS.normal_flag",
            "OBS.qualifier_cd",
        ]
        .into_iter()
        .find_map(|name| self.field(name))
    }

    /// When the observation was made: `OBS.observation_dttm`, or else
    /// `SVC.observation_dttm`.
    pub fn observed_at(&self) -> Option<&'a str> {
        self.field("OBS.observation_dttm").or_else(|| {
            self.service
                .and_then(|s| s.value_at("SVC.observation_dttm"))
        })
    }

    /// `PT.patient_id` of the enclosing patient.
    pub fn patient_id(&self) -> Option<&'a str> {
        self.subject
            .filter(|s| s.name() == "PT")
            .and_then(|s| s.value_at("PT.patient_id"))
    }

    /// `OPR.operator_id` of the service event.
    pub fn operator_id(&self) -> Option<&'a str> {
        self.service.and_then(|s| s.value_at("OPR/OPR.operator_id"))
    }

    /// The specimen type: the first element named `*.specimen_cd` or
    /// `*.specimen_type_cd` in the observation, its subject or its service.
    pub fn specimen(&self) -> Option<&'a str> {
        [Some(self.element), self.subject, self.service]
            .into_iter()
            .flatten()
            .flat_map(Element::descendants)
            .find(|e| {
                e.name()
                    .rsplit_once('.')
                    .is_some_and(|(_, field)| matches!(field, "specimen_cd" | "specimen_type_cd"))
            })
            .and_then(Element::value)
    }

    /// `CTC.name`, the control material of a quality control result.
    pub fn control_name(&self) -> Option<&'a str> {
        self.subject
            .filter(|s| s.name() == "CTC")
            .and_then(|s| s.value_at("CTC.name"))
    }

    /// `CTC.lot_number`, the lot of the control material.
    pub fn control_lot(&self) -> Option<&'a str> {
        self.subject
            .filter(|s| s.name() == "CTC")
            .and_then(|s| s.value_at("CTC.lot_number"))
    }
}

/// Device identification from a hello message (`HEL`, element `DEV`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub struct DeviceInfo {
    /// `DEV.device_id`.
    pub device_id: Option<String>,
    /// `DEV.vendor_id`.
    pub vendor_id: Option<String>,
    /// `DEV.model_id`.
    pub model_id: Option<String>,
    /// `DEV.serial_id`.
    pub serial_id: Option<String>,
    /// `DEV.manufacturer_name`.
    pub manufacturer_name: Option<String>,
    /// `DEV.hw_version`.
    pub hardware_version: Option<String>,
    /// `DEV.sw_version`.
    pub software_version: Option<String>,
    /// `DEV.device_name`.
    pub device_name: Option<String>,
}

/// Device status from a status message (`DST`, element `DST`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub struct DeviceStatus {
    /// `DST.status_dttm`.
    pub status_dttm: Option<String>,
    /// `DST.new_observations_qty`: observations not yet sent.
    pub new_observations: Option<u64>,
    /// `DST.new_events_qty`: events not yet sent.
    pub new_events: Option<u64>,
    /// `DST.operators_update_dttm`: when the device last received an
    /// operator list.
    pub operators_update_dttm: Option<String>,
    /// `DST.condition_cd`.
    pub condition: Option<String>,
}

/// The content of an acknowledgment (`ACK`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct AckInfo {
    /// `ACK.type_cd`: `AA`, `AE` or `AR`.
    pub ack_type: Option<AckType>,
    /// `ACK.ack_control_id`: the control ID of the acknowledged message.
    pub acked_control_id: Option<String>,
    /// `ACK.note_txt`.
    pub note: Option<String>,
}

/// Acknowledgment types (`ACK.type_cd`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AckType {
    /// `AA`: accepted.
    Accept,
    /// `AE`: error.
    Error,
    /// `AR`: rejected.
    Reject,
}

impl AckType {
    /// The two-letter code.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accept => "AA",
            Self::Error => "AE",
            Self::Reject => "AR",
        }
    }

    /// Parses a two-letter code.
    pub fn parse(code: &str) -> Option<Self> {
        match code.trim() {
            "AA" => Some(Self::Accept),
            "AE" => Some(Self::Error),
            "AR" => Some(Self::Reject),
            _ => None,
        }
    }
}

impl Message {
    /// The observations of the message, in document order. Every `OBS`
    /// element is returned with its nearest enclosing `SVC` and `PT`/`CTC`.
    pub fn observations(&self) -> Vec<Observation<'_>> {
        let mut found = Vec::new();
        collect_observations(self.root(), None, None, &mut found);
        found
    }

    /// Device identification, for a hello (`HEL`) message.
    pub fn device_info(&self) -> Option<DeviceInfo> {
        let dev = self.find("DEV")?;
        let value = |name: &str| dev.value_at(name).map(str::to_owned);
        Some(DeviceInfo {
            device_id: value("DEV.device_id"),
            vendor_id: value("DEV.vendor_id"),
            model_id: value("DEV.model_id"),
            serial_id: value("DEV.serial_id"),
            manufacturer_name: value("DEV.manufacturer_name"),
            hardware_version: value("DEV.hw_version"),
            software_version: value("DEV.sw_version"),
            device_name: value("DEV.device_name"),
        })
    }

    /// Device status, for a status (`DST`) message.
    pub fn device_status(&self) -> Option<DeviceStatus> {
        let dst = self.find("DST")?;
        let value = |name: &str| dst.value_at(name).map(str::to_owned);
        let quantity = |name: &str| dst.value_at(name).and_then(|v| v.trim().parse().ok());
        Some(DeviceStatus {
            status_dttm: value("DST.status_dttm"),
            new_observations: quantity("DST.new_observations_qty"),
            new_events: quantity("DST.new_events_qty"),
            operators_update_dttm: value("DST.operators_update_dttm"),
            condition: value("DST.condition_cd"),
        })
    }

    /// The acknowledgment content, for an `ACK` message.
    pub fn ack_info(&self) -> Option<AckInfo> {
        let ack = self.find("ACK")?;
        Some(AckInfo {
            ack_type: ack.value_at("ACK.type_cd").and_then(AckType::parse),
            acked_control_id: ack.value_at("ACK.ack_control_id").map(str::to_owned),
            note: ack.value_at("ACK.note_txt").map(str::to_owned),
        })
    }

    /// `EOT.topic_cd`, for an end-of-topic (`EOT`) message.
    pub fn end_of_topic(&self) -> Option<&str> {
        self.value("EOT/EOT.topic_cd")
    }
}

fn collect_observations<'a>(
    element: &'a Element,
    service: Option<&'a Element>,
    subject: Option<&'a Element>,
    found: &mut Vec<Observation<'a>>,
) {
    for child in element.elements() {
        match child.name() {
            "OBS" => {
                found.push(Observation {
                    element: child,
                    service,
                    subject,
                });
            }
            "SVC" => collect_observations(child, Some(child), None, found),
            "PT" | "CTC" => collect_observations(child, service, Some(child), found),
            _ => collect_observations(child, service, subject, found),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OBS: &str = r#"<OBS.R01>
  <HDR><HDR.control_id V="7"/><HDR.version_id V="POCT1"/></HDR>
  <SVC>
    <SVC.role_cd V="OBS"/>
    <SVC.observation_dttm V="2026-09-29T11:58:00+03:00"/>
    <PT>
      <PT.patient_id V="P-001"/>
      <OBS>
        <OBS.observation_id V="GLU" SN="LOCAL"/>
        <OBS.value V="5.4" U="mmol/L"/>
        <OBS.method_cd V="M"/>
        <OBS.interpretation_cd V="N"/>
        <OBS.specimen_cd V="BLD"/>
      </OBS>
      <OBS>
        <OBS.observation_id V="NA"/>
        <OBS.value V="140" U="mmol/L"/>
        <OBS.observation_dttm V="2026-09-29T11:59:00+03:00"/>
      </OBS>
    </PT>
    <OPR><OPR.operator_id V="OP7"/></OPR>
  </SVC>
  <SVC>
    <SVC.role_cd V="LQC"/>
    <CTC><CTC.name V="Level 1"/><CTC.lot_number V="L42"/>
      <OBS><OBS.observation_id V="GLU"/><OBS.value V="4.9" U="mmol/L"/></OBS>
    </CTC>
  </SVC>
</OBS.R01>"#;

    #[test]
    fn extracts_observations() {
        let message = Message::parse(OBS.as_bytes()).unwrap();
        let observations = message.observations();
        assert_eq!(observations.len(), 3);

        let glucose = observations[0];
        assert_eq!(glucose.kind(), ObservationKind::Patient);
        assert_eq!(glucose.observation_id(), Some("GLU"));
        assert_eq!(glucose.coding_system(), Some("LOCAL"));
        assert_eq!(
            (glucose.value(), glucose.unit()),
            (Some("5.4"), Some("mmol/L"))
        );
        assert_eq!(glucose.method(), Some("M"));
        assert_eq!(glucose.interpretation(), Some("N"));
        assert_eq!(glucose.observed_at(), Some("2026-09-29T11:58:00+03:00"));
        assert_eq!(glucose.patient_id(), Some("P-001"));
        assert_eq!(glucose.operator_id(), Some("OP7"));
        assert_eq!(glucose.specimen(), Some("BLD"));
        assert_eq!(glucose.control_name(), None);

        let sodium = observations[1];
        assert_eq!(sodium.observed_at(), Some("2026-09-29T11:59:00+03:00"));
        assert_eq!(sodium.interpretation(), None);

        let control = observations[2];
        assert_eq!(control.kind(), ObservationKind::QualityControl);
        assert_eq!(control.patient_id(), None);
        assert_eq!(control.control_name(), Some("Level 1"));
        assert_eq!(control.control_lot(), Some("L42"));
        assert_eq!(control.value(), Some("4.9"));
    }

    #[test]
    fn infers_kind_without_role() {
        let message =
            Message::parse(b"<OBS.R01><SVC><CTC><OBS/></CTC><PT><OBS/></PT><OBS/></SVC></OBS.R01>")
                .unwrap();
        let kinds: Vec<_> = message
            .observations()
            .iter()
            .map(Observation::kind)
            .collect();
        assert_eq!(
            kinds,
            [
                ObservationKind::QualityControl,
                ObservationKind::Patient,
                ObservationKind::Other
            ]
        );
    }

    #[test]
    fn reads_hello_status_and_ack() {
        let hello = Message::parse(
            br#"<HEL.R01><HDR><HDR.control_id V="1"/></HDR><DEV><DEV.device_id V="D1"/><DEV.vendor_id V="ACME"/><DEV.model_id V="G1"/><DEV.serial_id V="S9"/><DEV.sw_version V="2.1"/></DEV></HEL.R01>"#,
        )
        .unwrap();
        let info = hello.device_info().unwrap();
        assert_eq!(info.device_id.as_deref(), Some("D1"));
        assert_eq!(info.vendor_id.as_deref(), Some("ACME"));
        assert_eq!(info.software_version.as_deref(), Some("2.1"));
        assert_eq!(info.hardware_version, None);

        let status = Message::parse(
            br#"<DST.R01><DST><DST.new_observations_qty V=" 12 "/><DST.new_events_qty V="x"/><DST.condition_cd V="R"/></DST></DST.R01>"#,
        )
        .unwrap()
        .device_status()
        .unwrap();
        assert_eq!(status.new_observations, Some(12));
        assert_eq!(status.new_events, None);
        assert_eq!(status.condition.as_deref(), Some("R"));

        let ack = Message::parse(
            br#"<ACK.R01><ACK><ACK.type_cd V="AE"/><ACK.ack_control_id V="9"/><ACK.note_txt V="bad"/></ACK></ACK.R01>"#,
        )
        .unwrap()
        .ack_info()
        .unwrap();
        assert_eq!(ack.ack_type, Some(AckType::Error));
        assert_eq!(ack.acked_control_id.as_deref(), Some("9"));
        assert_eq!(ack.note.as_deref(), Some("bad"));

        assert!(hello.device_status().is_none());
        assert!(hello.ack_info().is_none());
        assert_eq!(AckType::parse("AR"), Some(AckType::Reject));
        assert_eq!(AckType::Accept.as_str(), "AA");
    }
}
