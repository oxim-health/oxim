//! Normalizers and encoders never panic, whatever the parsed input.

#![allow(clippy::unwrap_used)]

use oxim_mapping::hl7::Hl7Encoding;
use oxim_mapping::{astm, hl7, poct1a};
use oxim_model::{ClinicalContent, MessageId, Timestamp};
use proptest::prelude::*;

fn bytes(alphabet: &'static [u8]) -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(
        prop_oneof![5 => prop::sample::select(alphabet.to_vec()), 1 => any::<u8>()],
        0..400,
    )
}

fn check_encoders(content: &ClinicalContent) {
    let id = MessageId::from_parts(1, 1);
    let at = Timestamp::from_unix_nanos(0);
    if let Ok(message) = hl7::encode_results(content, &Hl7Encoding::default(), id, at) {
        assert!(oxim_hl7::Message::parse(&message.to_bytes()).is_ok());
    }
    if let Ok(message) = hl7::encode_orders(content, &Hl7Encoding::default(), id, at) {
        assert!(oxim_hl7::Message::parse(&message.to_bytes()).is_ok());
    }
    if let Ok(message) = astm::encode_orders(content, &astm::AstmEncoding::default(), at) {
        assert!(oxim_astm::Message::parse(&message.to_bytes()).is_ok());
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    #[test]
    fn astm_normalizer_never_panics(body in bytes(b"|\\^&\rHPORCQL0123456789.<>-^ ")) {
        let mut input = b"H|\\^&\r".to_vec();
        input.extend(body);
        if let Ok(message) = oxim_astm::Message::parse(&input)
            && let Ok(content) = astm::normalize(&message)
        {
            check_encoders(&content);
        }
    }

    #[test]
    fn hl7_normalizer_never_panics(
        kind in prop::sample::select(vec!["ORU^R01", "OUL^R22", "OML^O21", "ORM^O01", "QBP^Q11", "QRY^R02", "ADT^A01"]),
        body in bytes(b"|^~\\&\rPIDORCOBXNTESPMQRDQPD0123456789.<>-^ "),
    ) {
        let mut input = format!("MSH|^~\\&|A||B||20260101||{kind}|1|P|2.5.1\r").into_bytes();
        input.extend(body);
        if let Ok(message) = oxim_hl7::Message::parse(&input)
            && let Ok(content) = hl7::normalize(&message)
        {
            check_encoders(&content);
        }
    }

    #[test]
    fn poct_normalizer_never_panics(value in "[ -~]{0,12}", unit in "[a-zA-Z/%]{0,6}", role in prop::sample::select(vec!["OBS", "LQC", "CAL", "PRF", "XX"])) {
        let xml = format!(
            r#"<OBS.R01><HDR><HDR.control_id V="1"/></HDR><SVC><SVC.role_cd V="{role}"/><PT><OBS><OBS.observation_id V="GLU"/><OBS.value V="{}" U="{unit}"/></OBS></PT></SVC></OBS.R01>"#,
            value.replace('&', "&amp;").replace('<', "&lt;").replace('"', "&quot;")
        );
        if let Ok(message) = oxim_poct1a::Message::parse(xml.as_bytes())
            && let Ok(content) = poct1a::normalize(&message)
        {
            check_encoders(&content);
        }
    }
}
