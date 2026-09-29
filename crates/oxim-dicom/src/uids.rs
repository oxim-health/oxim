//! Well-known UIDs and UID helpers.

use dicom_core::dictionary::UidDictionary;
use dicom_dictionary_std::StandardSopClassDictionary;
use dicom_dictionary_std::uids as std_uids;

/// The Verification SOP Class, used by C-ECHO.
pub const VERIFICATION: &str = "1.2.840.10008.1.1";
/// Implicit VR Little Endian, the default transfer syntax.
pub const IMPLICIT_VR_LITTLE_ENDIAN: &str = "1.2.840.10008.1.2";
/// Explicit VR Little Endian.
pub const EXPLICIT_VR_LITTLE_ENDIAN: &str = "1.2.840.10008.1.2.1";
/// Explicit VR Big Endian (retired, still sent by old modalities).
pub const EXPLICIT_VR_BIG_ENDIAN: &str = "1.2.840.10008.1.2.2";
/// The Implementation Class UID written by OXIM, a UUID-derived UID
/// (PS3.5 B.2).
pub const IMPLEMENTATION_CLASS_UID: &str = "2.25.328597613184474120346224383219384307893";
/// The Implementation Version Name written by OXIM.
pub const IMPLEMENTATION_VERSION_NAME: &str = concat!("OXIM_", env!("CARGO_PKG_VERSION"));

/// Patient Root Query/Retrieve Information Model - FIND.
pub const PATIENT_ROOT_FIND: &str = "1.2.840.10008.5.1.4.1.2.1.1";
/// Patient Root Query/Retrieve Information Model - MOVE.
pub const PATIENT_ROOT_MOVE: &str = "1.2.840.10008.5.1.4.1.2.1.2";
/// Patient Root Query/Retrieve Information Model - GET.
pub const PATIENT_ROOT_GET: &str = "1.2.840.10008.5.1.4.1.2.1.3";
/// Study Root Query/Retrieve Information Model - FIND.
pub const STUDY_ROOT_FIND: &str = "1.2.840.10008.5.1.4.1.2.2.1";
/// Study Root Query/Retrieve Information Model - MOVE.
pub const STUDY_ROOT_MOVE: &str = "1.2.840.10008.5.1.4.1.2.2.2";
/// Study Root Query/Retrieve Information Model - GET.
pub const STUDY_ROOT_GET: &str = "1.2.840.10008.5.1.4.1.2.2.3";
/// Modality Worklist Information Model - FIND.
pub const MODALITY_WORKLIST_FIND: &str = "1.2.840.10008.5.1.4.31";
/// Modality Performed Procedure Step SOP Class.
pub const MPPS: &str = "1.2.840.10008.3.1.2.3.3";
/// Storage Commitment Push Model SOP Class.
pub const STORAGE_COMMITMENT_PUSH: &str = "1.2.840.10008.1.20.1";
/// The well-known SOP instance of the Storage Commitment Push Model.
pub const STORAGE_COMMITMENT_INSTANCE: &str = "1.2.840.10008.1.20.1.1";

/// A new UID under the `2.25` arc: a random 128-bit number (PS3.5 B.2).
pub fn generate() -> String {
    use ring::rand::SecureRandom;
    let mut bytes = [0u8; 16];
    if ring::rand::SystemRandom::new().fill(&mut bytes).is_err() {
        // The system generator does not fail on supported platforms; fall
        // back to the clock so a UID is still produced.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        bytes = nanos.to_be_bytes();
    }
    format!("2.25.{}", u128::from_be_bytes(bytes))
}

/// A UID under the `2.25` arc derived from `seed` with SHA-256, so the same
/// seed always gives the same UID.
pub fn derived(seed: &str) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, seed.as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest.as_ref()[..16]);
    format!("2.25.{}", u128::from_be_bytes(bytes))
}

/// Whether `uid` is a syntactically valid UID: at most 64 characters,
/// numeric components separated by dots, without leading zeros.
pub fn is_valid_uid(uid: &str) -> bool {
    !uid.is_empty()
        && uid.len() <= 64
        && uid.split('.').all(|component| {
            !component.is_empty()
                && component.bytes().all(|b| b.is_ascii_digit())
                && (component == "0" || !component.starts_with('0'))
        })
}

/// Resolves a SOP class given as a UID or as a keyword such as
/// `CTImageStorage`.
pub fn resolve_sop_class(name: &str) -> Option<String> {
    if is_valid_uid(name) {
        return Some(name.to_owned());
    }
    StandardSopClassDictionary
        .by_keyword(name)
        .map(|entry| entry.uid.to_owned())
}

/// The storage SOP classes a `dicom-scp` source accepts by default: the
/// image, waveform, structured report, presentation state, RT and
/// document storage classes in current use.
pub const DEFAULT_STORAGE_SOP_CLASSES: &[&str] = &[
    // Radiography and mammography
    std_uids::COMPUTED_RADIOGRAPHY_IMAGE_STORAGE,
    std_uids::DIGITAL_X_RAY_IMAGE_STORAGE_FOR_PRESENTATION,
    std_uids::DIGITAL_X_RAY_IMAGE_STORAGE_FOR_PROCESSING,
    std_uids::DIGITAL_MAMMOGRAPHY_X_RAY_IMAGE_STORAGE_FOR_PRESENTATION,
    std_uids::DIGITAL_MAMMOGRAPHY_X_RAY_IMAGE_STORAGE_FOR_PROCESSING,
    std_uids::DIGITAL_INTRA_ORAL_X_RAY_IMAGE_STORAGE_FOR_PRESENTATION,
    std_uids::DIGITAL_INTRA_ORAL_X_RAY_IMAGE_STORAGE_FOR_PROCESSING,
    std_uids::BREAST_TOMOSYNTHESIS_IMAGE_STORAGE,
    std_uids::BREAST_PROJECTION_X_RAY_IMAGE_STORAGE_FOR_PRESENTATION,
    std_uids::BREAST_PROJECTION_X_RAY_IMAGE_STORAGE_FOR_PROCESSING,
    // Angiography and fluoroscopy
    std_uids::X_RAY_ANGIOGRAPHIC_IMAGE_STORAGE,
    std_uids::ENHANCED_XA_IMAGE_STORAGE,
    std_uids::X_RAY_RADIOFLUOROSCOPIC_IMAGE_STORAGE,
    std_uids::ENHANCED_XRF_IMAGE_STORAGE,
    std_uids::X_RAY3_D_ANGIOGRAPHIC_IMAGE_STORAGE,
    std_uids::X_RAY3_D_CRANIOFACIAL_IMAGE_STORAGE,
    // CT, MR, nuclear medicine, PET, ultrasound
    std_uids::CT_IMAGE_STORAGE,
    std_uids::ENHANCED_CT_IMAGE_STORAGE,
    std_uids::LEGACY_CONVERTED_ENHANCED_CT_IMAGE_STORAGE,
    std_uids::MR_IMAGE_STORAGE,
    std_uids::ENHANCED_MR_IMAGE_STORAGE,
    std_uids::ENHANCED_MR_COLOR_IMAGE_STORAGE,
    std_uids::LEGACY_CONVERTED_ENHANCED_MR_IMAGE_STORAGE,
    std_uids::MR_SPECTROSCOPY_STORAGE,
    std_uids::NUCLEAR_MEDICINE_IMAGE_STORAGE,
    std_uids::POSITRON_EMISSION_TOMOGRAPHY_IMAGE_STORAGE,
    std_uids::ENHANCED_PET_IMAGE_STORAGE,
    std_uids::LEGACY_CONVERTED_ENHANCED_PET_IMAGE_STORAGE,
    std_uids::ULTRASOUND_IMAGE_STORAGE,
    std_uids::ULTRASOUND_MULTI_FRAME_IMAGE_STORAGE,
    std_uids::ENHANCED_US_VOLUME_STORAGE,
    std_uids::PARAMETRIC_MAP_STORAGE,
    std_uids::RAW_DATA_STORAGE,
    // Secondary capture and visible light
    std_uids::SECONDARY_CAPTURE_IMAGE_STORAGE,
    std_uids::MULTI_FRAME_SINGLE_BIT_SECONDARY_CAPTURE_IMAGE_STORAGE,
    std_uids::MULTI_FRAME_GRAYSCALE_BYTE_SECONDARY_CAPTURE_IMAGE_STORAGE,
    std_uids::MULTI_FRAME_GRAYSCALE_WORD_SECONDARY_CAPTURE_IMAGE_STORAGE,
    std_uids::MULTI_FRAME_TRUE_COLOR_SECONDARY_CAPTURE_IMAGE_STORAGE,
    std_uids::VL_ENDOSCOPIC_IMAGE_STORAGE,
    std_uids::VIDEO_ENDOSCOPIC_IMAGE_STORAGE,
    std_uids::VL_MICROSCOPIC_IMAGE_STORAGE,
    std_uids::VIDEO_MICROSCOPIC_IMAGE_STORAGE,
    std_uids::VL_SLIDE_COORDINATES_MICROSCOPIC_IMAGE_STORAGE,
    std_uids::VL_PHOTOGRAPHIC_IMAGE_STORAGE,
    std_uids::VIDEO_PHOTOGRAPHIC_IMAGE_STORAGE,
    std_uids::VL_WHOLE_SLIDE_MICROSCOPY_IMAGE_STORAGE,
    std_uids::DERMOSCOPIC_PHOTOGRAPHY_IMAGE_STORAGE,
    std_uids::OPHTHALMIC_PHOTOGRAPHY8_BIT_IMAGE_STORAGE,
    std_uids::OPHTHALMIC_PHOTOGRAPHY16_BIT_IMAGE_STORAGE,
    std_uids::OPHTHALMIC_TOMOGRAPHY_IMAGE_STORAGE,
    // Radiotherapy
    std_uids::RT_IMAGE_STORAGE,
    std_uids::RT_DOSE_STORAGE,
    std_uids::RT_STRUCTURE_SET_STORAGE,
    std_uids::RT_PLAN_STORAGE,
    std_uids::RT_ION_PLAN_STORAGE,
    std_uids::RT_BEAMS_TREATMENT_RECORD_STORAGE,
    std_uids::RT_BRACHY_TREATMENT_RECORD_STORAGE,
    std_uids::RT_TREATMENT_SUMMARY_RECORD_STORAGE,
    std_uids::RT_ION_BEAMS_TREATMENT_RECORD_STORAGE,
    // Registration, segmentation and presentation states
    std_uids::SPATIAL_REGISTRATION_STORAGE,
    std_uids::DEFORMABLE_SPATIAL_REGISTRATION_STORAGE,
    std_uids::SPATIAL_FIDUCIALS_STORAGE,
    std_uids::SEGMENTATION_STORAGE,
    std_uids::SURFACE_SEGMENTATION_STORAGE,
    std_uids::REAL_WORLD_VALUE_MAPPING_STORAGE,
    std_uids::GRAYSCALE_SOFTCOPY_PRESENTATION_STATE_STORAGE,
    std_uids::COLOR_SOFTCOPY_PRESENTATION_STATE_STORAGE,
    std_uids::PSEUDO_COLOR_SOFTCOPY_PRESENTATION_STATE_STORAGE,
    std_uids::BLENDING_SOFTCOPY_PRESENTATION_STATE_STORAGE,
    // Structured reports and documents
    std_uids::BASIC_TEXT_SR_STORAGE,
    std_uids::ENHANCED_SR_STORAGE,
    std_uids::COMPREHENSIVE_SR_STORAGE,
    std_uids::COMPREHENSIVE3_DSR_STORAGE,
    std_uids::EXTENSIBLE_SR_STORAGE,
    std_uids::KEY_OBJECT_SELECTION_DOCUMENT_STORAGE,
    std_uids::MAMMOGRAPHY_CADSR_STORAGE,
    std_uids::CHEST_CADSR_STORAGE,
    std_uids::X_RAY_RADIATION_DOSE_SR_STORAGE,
    std_uids::RADIOPHARMACEUTICAL_RADIATION_DOSE_SR_STORAGE,
    std_uids::PATIENT_RADIATION_DOSE_SR_STORAGE,
    std_uids::ACQUISITION_CONTEXT_SR_STORAGE,
    std_uids::PROCEDURE_LOG_STORAGE,
    std_uids::ENCAPSULATED_PDF_STORAGE,
    std_uids::ENCAPSULATED_CDA_STORAGE,
    std_uids::ENCAPSULATED_STL_STORAGE,
    // Waveforms
    std_uids::TWELVE_LEAD_ECG_WAVEFORM_STORAGE,
    std_uids::GENERAL_ECG_WAVEFORM_STORAGE,
    std_uids::AMBULATORY_ECG_WAVEFORM_STORAGE,
    std_uids::HEMODYNAMIC_WAVEFORM_STORAGE,
    std_uids::CARDIAC_ELECTROPHYSIOLOGY_WAVEFORM_STORAGE,
    std_uids::BASIC_VOICE_AUDIO_WAVEFORM_STORAGE,
    std_uids::GENERAL_AUDIO_WAVEFORM_STORAGE,
    std_uids::ARTERIAL_PULSE_WAVEFORM_STORAGE,
    std_uids::RESPIRATORY_WAVEFORM_STORAGE,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_uids() {
        for good in [VERIFICATION, IMPLEMENTATION_CLASS_UID, "0.1", "2.25.0"] {
            assert!(is_valid_uid(good), "{good}");
        }
        for bad in ["", "1..2", "1.02", "1.2.", "1.a", &"1.".repeat(40)] {
            assert!(!is_valid_uid(bad), "{bad}");
        }
        assert!(IMPLEMENTATION_VERSION_NAME.len() <= 16);
        assert!(
            DEFAULT_STORAGE_SOP_CLASSES
                .iter()
                .all(|uid| is_valid_uid(uid))
        );
    }

    #[test]
    fn resolves_sop_class_keywords() {
        assert_eq!(
            resolve_sop_class("CTImageStorage").as_deref(),
            Some("1.2.840.10008.5.1.4.1.1.2")
        );
        assert_eq!(resolve_sop_class("1.2.3").as_deref(), Some("1.2.3"));
        assert_eq!(resolve_sop_class("NoSuchStorage"), None);
    }
}
