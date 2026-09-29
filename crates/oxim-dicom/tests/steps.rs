//! The DICOM filter and transformers, directly and in a channel.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::Arc;

use common::{
    CT_IMAGE_STORAGE, Recorder, Synthetic, context, deploy, engine, free_port, raw, scu,
    store_when_ready, text, wait_until,
};
use oxim_core::{Filter, Transformer};
use oxim_dicom::{
    Deidentify, DeidentifySettings, DicomObject, DicomSet, DicomSetSettings, DicomTagFilter,
    DicomTagFilterSettings, part10, uids,
};

const SECRET: &str = "synthetic-test-secret-0123456789";

fn filter(json: serde_json::Value) -> Result<DicomTagFilter, oxim_core::EngineError> {
    DicomTagFilter::new(serde_json::from_value::<DicomTagFilterSettings>(json).unwrap())
}

fn deidentify(json: serde_json::Value) -> Deidentify {
    Deidentify::new(serde_json::from_value::<DeidentifySettings>(json).unwrap()).unwrap()
}

fn apply(transformer: &dyn Transformer, bytes: Vec<u8>) -> DicomObject {
    let mut context = context(bytes);
    transformer.apply(&mut context).unwrap();
    DicomObject::parse(&raw(&context)).unwrap()
}

#[test]
fn filters_by_attribute() {
    let ct = context(Synthetic::ct(1).bytes());
    let mr = context(Synthetic::ct(2).with_modality("MR").bytes());
    let accepts = |filter: &DicomTagFilter, context| filter.accept(context).unwrap();

    let is_ct = filter(serde_json::json!({"tag": "Modality", "equals": "CT"})).unwrap();
    assert!(accepts(&is_ct, &ct));
    assert!(!accepts(&is_ct, &mr));
    let by_tag = filter(serde_json::json!({"tag": "(0008,0060)", "in": ["MR", "US"]})).unwrap();
    assert!(!accepts(&by_tag, &ct));
    assert!(accepts(&by_tag, &mr));
    let not_ct =
        filter(serde_json::json!({"tag": "00080060", "equals": "CT", "negate": true})).unwrap();
    assert!(!accepts(&not_ct, &ct));
    let nested = filter(serde_json::json!({
        "tag": "ReferencedImageSequence[0].ReferencedSOPClassUID",
        "equals": CT_IMAGE_STORAGE,
    }))
    .unwrap();
    assert!(accepts(&nested, &ct));
    let private = filter(serde_json::json!({"tag": "(0009,1001)", "exists": true})).unwrap();
    assert!(accepts(&private, &ct));
    let absent = filter(serde_json::json!({"tag": "OtherPatientNames", "exists": false})).unwrap();
    assert!(accepts(&absent, &ct));
    let sequence =
        filter(serde_json::json!({"tag": "ReferencedImageSequence", "exists": true})).unwrap();
    assert!(accepts(&sequence, &ct));

    assert!(filter(serde_json::json!({"tag": "Modality"})).is_err());
    assert!(
        filter(serde_json::json!({"tag": "Modality", "equals": "CT", "exists": true})).is_err()
    );
    assert!(filter(serde_json::json!({"tag": "NoSuchKeyword", "equals": "x"})).is_err());

    // Not DICOM: the step fails instead of guessing.
    let garbage = context(b"not a DICOM object".to_vec());
    assert!(is_ct.accept(&garbage).is_err());
}

#[test]
fn sets_and_removes_attributes() {
    let settings: DicomSetSettings = serde_json::from_value(serde_json::json!({
        "set": {
            "InstitutionName": "Synthetic Imaging Center",
            "(0008,1030)": "Routed study",
            "RequestAttributesSequence[0].AccessionNumber": "ACC-99",
            "Rows": 8,
            "OtherPatientIDs": "A\\B",
            "(0011,1001)": {"value": "custom", "vr": "LO"},
        },
        "remove": ["PatientBirthDate", "ReferencedImageSequence[0].ReferencedSOPClassUID"],
        "remove_private": true,
    }))
    .unwrap();
    let step = DicomSet::new(settings).unwrap();
    for transfer_syntax in [
        uids::EXPLICIT_VR_LITTLE_ENDIAN,
        uids::IMPLICIT_VR_LITTLE_ENDIAN,
        uids::EXPLICIT_VR_BIG_ENDIAN,
    ] {
        let object = Synthetic::ct(1).with_transfer_syntax(transfer_syntax);
        let result = apply(&step, object.bytes());
        assert_eq!(result.meta.transfer_syntax, transfer_syntax);
        assert_eq!(
            text(&result, "InstitutionName").as_deref(),
            Some("Synthetic Imaging Center")
        );
        assert_eq!(
            text(&result, "StudyDescription").as_deref(),
            Some("Routed study")
        );
        assert_eq!(
            text(&result, "RequestAttributesSequence[0].AccessionNumber").as_deref(),
            Some("ACC-99")
        );
        assert_eq!(text(&result, "Rows").as_deref(), Some("8"));
        assert_eq!(text(&result, "OtherPatientIDs").as_deref(), Some("A\\B"));
        assert_eq!(text(&result, "PatientBirthDate"), None);
        assert_eq!(
            text(&result, "ReferencedImageSequence[0].ReferencedSOPClassUID"),
            None
        );
        // remove_private runs first; the new private attribute stays (as
        // UN bytes when the syntax does not record its VR).
        assert_eq!(text(&result, "(0009,1001)"), None);
        let private = oxim_dicom::parse_selector("(0011,1001)").unwrap();
        assert!(result.contains(&private));
        if transfer_syntax != uids::IMPLICIT_VR_LITTLE_ENDIAN {
            assert_eq!(text(&result, "(0011,1001)").as_deref(), Some("custom"));
        }
        // Untouched attributes survive.
        assert_eq!(text(&result, "PatientID").as_deref(), Some("SYN-0001"));
        assert_eq!(result.meta.sop_instance_uid, object.instance_uid);
    }

    let invalid = |json: serde_json::Value| {
        DicomSet::new(serde_json::from_value::<DicomSetSettings>(json).unwrap()).is_err()
    };
    assert!(invalid(serde_json::json!({})));
    assert!(invalid(
        serde_json::json!({"set": {"(0011,1001)": "no VR for a private tag"}})
    ));
    assert!(invalid(
        serde_json::json!({"set": {"Rows": "not a number"}})
    ));
    assert!(invalid(
        serde_json::json!({"set": {"ReferencedImageSequence": "x"}})
    ));
    assert!(invalid(
        serde_json::json!({"set": {"Rows": {"value": 1, "vr": "XX"}}})
    ));
}

#[test]
fn deidentifies_a_study_consistently() {
    let step = deidentify(serde_json::json!({"secret": SECRET, "keep": ["PatientSex"]}));
    let first = Synthetic::ct(1);
    let second = Synthetic::ct(2);
    let a = apply(&step, first.bytes());
    let b = apply(&step, second.bytes());

    // One study stays one study, under new UIDs.
    let study_a = text(&a, "StudyInstanceUID").unwrap();
    assert_eq!(Some(&study_a), text(&b, "StudyInstanceUID").as_ref());
    assert_ne!(study_a, first.study_uid);
    assert!(study_a.starts_with("2.25."));
    assert!(uids::is_valid_uid(&study_a));
    assert_eq!(text(&a, "SeriesInstanceUID"), text(&b, "SeriesInstanceUID"));
    assert_eq!(
        text(&a, "FrameOfReferenceUID"),
        text(&b, "FrameOfReferenceUID")
    );
    let instance_a = text(&a, "SOPInstanceUID").unwrap();
    let instance_b = text(&b, "SOPInstanceUID").unwrap();
    assert_ne!(instance_a, instance_b);
    assert_ne!(instance_a, first.instance_uid);
    assert_eq!(a.meta.sop_instance_uid, instance_a);
    let reference = text(&a, "ReferencedImageSequence[0].ReferencedSOPInstanceUID").unwrap();
    assert_ne!(reference, format!("{}.99", first.series_uid));
    assert_eq!(
        Some(reference),
        text(&b, "ReferencedImageSequence[0].ReferencedSOPInstanceUID")
    );
    // Class UIDs are not instance identifiers.
    assert_eq!(text(&a, "SOPClassUID").as_deref(), Some(CT_IMAGE_STORAGE));
    assert_eq!(
        text(&a, "ReferencedImageSequence[0].ReferencedSOPClassUID").as_deref(),
        Some(CT_IMAGE_STORAGE)
    );
    assert_eq!(a.meta.sop_class_uid, CT_IMAGE_STORAGE);

    // The same patient gets the same pseudonym.
    let pseudonym = text(&a, "PatientID").unwrap();
    assert!(pseudonym.starts_with("ANON-"), "{pseudonym}");
    assert_eq!(text(&a, "PatientName"), Some(pseudonym.clone()));
    assert_eq!(text(&b, "PatientID"), Some(pseudonym));

    // Basic Profile actions.
    assert_eq!(text(&a, "PatientBirthDate").as_deref(), Some(""));
    assert_eq!(text(&a, "StudyDate").as_deref(), Some(""));
    assert_eq!(text(&a, "AccessionNumber").as_deref(), Some(""));
    assert_eq!(text(&a, "InstitutionName").as_deref(), Some(""));
    assert_eq!(text(&a, "ReferringPhysicianName").as_deref(), Some(""));
    assert_eq!(text(&a, "StudyDescription"), None);
    assert_eq!(text(&a, "(0009,1001)"), None);
    assert_eq!(text(&a, "(0009,0010)"), None);
    assert_eq!(text(&a, "PatientSex").as_deref(), Some("O"));
    assert_eq!(text(&a, "Modality").as_deref(), Some("CT"));
    assert_eq!(text(&a, "PatientIdentityRemoved").as_deref(), Some("YES"));
    assert_eq!(
        text(&a, "DeidentificationMethodCodeSequence[0].CodeValue").as_deref(),
        Some("113100")
    );
    assert_eq!(
        text(&a, "LongitudinalTemporalInformationModified").as_deref(),
        Some("REMOVED")
    );
    assert_eq!(a.meta.sending_ae_title, None);
    assert_eq!(a.meta.source_ae_title, None);

    // Deterministic across instances with the same secret, and only then.
    let again = apply(
        &deidentify(serde_json::json!({"secret": SECRET, "keep": ["PatientSex"]})),
        first.bytes(),
    );
    assert_eq!(text(&again, "SOPInstanceUID"), Some(instance_a.clone()));
    let other = apply(
        &deidentify(serde_json::json!({"secret": "another-synthetic-secret-42"})),
        first.bytes(),
    );
    assert_ne!(text(&other, "SOPInstanceUID"), Some(instance_a));
    assert_eq!(text(&other, "PatientSex").as_deref(), Some(""));

    // The result is a valid Part 10 object in the original syntax.
    let bytes = a.to_bytes().unwrap();
    assert_eq!(
        part10::parse(&bytes).unwrap().meta.transfer_syntax,
        uids::EXPLICIT_VR_LITTLE_ENDIAN
    );
}

#[test]
fn shifts_dates_and_keeps_times() {
    let step = deidentify(serde_json::json!({"secret": SECRET, "date_shift_days": -10}));
    let result = apply(&step, Synthetic::ct(1).bytes());
    assert_eq!(text(&result, "StudyDate").as_deref(), Some("20240219"));
    assert_eq!(
        text(&result, "PatientBirthDate").as_deref(),
        Some("19800105")
    );
    assert_eq!(text(&result, "StudyTime").as_deref(), Some("101500"));
    assert_eq!(
        text(&result, "LongitudinalTemporalInformationModified").as_deref(),
        Some("MODIFIED")
    );
    assert_eq!(
        text(&result, "DeidentificationMethodCodeSequence[1].CodeValue").as_deref(),
        Some("113107")
    );

    // A per-patient shift is the same for every object of the patient.
    let step = deidentify(serde_json::json!({"secret": SECRET, "date_shift_max_days": 365}));
    let a = apply(&step, Synthetic::ct(1).bytes());
    let b = apply(&step, Synthetic::ct(2).bytes());
    assert_eq!(text(&a, "StudyDate"), text(&b, "StudyDate"));
    assert_eq!(text(&a, "StudyDate").unwrap().len(), 8);
}

#[test]
fn rejects_burned_in_annotation_unless_allowed() {
    let settings: DicomSetSettings =
        serde_json::from_value(serde_json::json!({"set": {"BurnedInAnnotation": "YES"}})).unwrap();
    let marked = apply(&DicomSet::new(settings).unwrap(), Synthetic::ct(1).bytes())
        .to_bytes()
        .unwrap();
    let strict = deidentify(serde_json::json!({"secret": SECRET}));
    let error = strict.apply(&mut context(marked.clone())).unwrap_err();
    assert!(error.message.contains("burned-in"), "{error}");
    let lenient =
        deidentify(serde_json::json!({"secret": SECRET, "allow_burned_in_annotation": true}));
    assert!(lenient.apply(&mut context(marked)).is_ok());
}

#[tokio::test(flavor = "multi_thread")]
async fn filters_and_deidentifies_in_a_channel() {
    let recorder = Arc::new(Recorder::default());
    let engine = engine(recorder.clone()).await;
    let port = free_port();
    deploy(
        &engine,
        &format!(
            "id: research\nsource:\n  type: dicom-scp\n  data_type: dicom\n  settings:\n    listen: 127.0.0.1:{port}\nfilters:\n  - type: dicom-tag\n    tag: Modality\n    equals: CT\ntransformers:\n  - type: dicom-deidentify\n    secret: {SECRET}\ndestinations:\n  - id: out\n    type: recorder\n"
        ),
    )
    .await;
    let modality = scu(port, "MODALITY", "OXIM");
    let mr = Synthetic::ct(7).with_modality("MR");
    assert_eq!(store_when_ready(&modality, &mr.bytes()).await.status, 0);
    let ct = Synthetic::ct(8);
    assert_eq!(store_when_ready(&modality, &ct.bytes()).await.status, 0);

    wait_until("the CT object is delivered", || {
        recorder.payloads().len() == 1
    })
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let payloads = recorder.payloads();
    assert_eq!(payloads.len(), 1, "the MR object is filtered");
    let delivered = DicomObject::parse(&payloads[0]).unwrap();
    assert_eq!(text(&delivered, "Modality").as_deref(), Some("CT"));
    assert_ne!(text(&delivered, "SOPInstanceUID").unwrap(), ct.instance_uid);
    assert!(text(&delivered, "PatientID").unwrap().starts_with("ANON-"));
    engine.shutdown().await;
}
