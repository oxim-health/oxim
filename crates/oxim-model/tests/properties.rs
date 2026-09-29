//! Property tests for identifiers, times and decimals.

use oxim_model::{ClinicalDateTime, Decimal, MessageId, Timestamp};
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(3000))]

    #[test]
    fn message_ids_round_trip(value in any::<u128>()) {
        let id = MessageId::from_u128(value);
        prop_assert_eq!(id.to_string().parse::<MessageId>().unwrap(), id);
    }

    #[test]
    fn message_id_text_sorts_like_the_value(a in any::<u128>(), b in any::<u128>()) {
        let (x, y) = (MessageId::from_u128(a), MessageId::from_u128(b));
        prop_assert_eq!(x.cmp(&y), x.to_string().cmp(&y.to_string()));
    }

    #[test]
    fn timestamps_round_trip_through_rfc3339(nanos in -9_000_000_000_000_000_000i64..9_000_000_000_000_000_000) {
        let ts = Timestamp::from_unix_nanos(nanos);
        prop_assert_eq!(ts.to_string().parse::<Timestamp>().unwrap(), ts);
    }

    #[test]
    fn clinical_times_round_trip(
        nanos in -2_000_000_000_000_000_000i64..7_000_000_000_000_000_000,
        offset in -720i16..=840,
        cut in 0usize..7,
    ) {
        let full = ClinicalDateTime::from_timestamp(Timestamp::from_unix_nanos(nanos), offset).unwrap();
        let hl7 = full.to_hl7();
        // Truncate to a random precision and keep the offset.
        let digits_end = hl7.find('.').or_else(|| hl7.find(['+', '-'])).unwrap();
        let keep = [4, 6, 8, 10, 12, 14, digits_end][cut].min(digits_end);
        let offset_part = &hl7[hl7.rfind(['+', '-']).unwrap()..];
        let text = if cut == 6 { hl7.clone() } else { format!("{}{offset_part}", &hl7[..keep]) };
        let parsed = ClinicalDateTime::parse_hl7(&text).unwrap();
        prop_assert_eq!(parsed.to_hl7(), text.clone());
        prop_assert_eq!(ClinicalDateTime::parse_iso(&parsed.to_iso()).unwrap().to_iso(), parsed.to_iso());
    }

    #[test]
    fn parsers_never_panic(text in "\\PC{0,40}") {
        let _ = ClinicalDateTime::parse_hl7(&text);
        let _ = ClinicalDateTime::parse_iso(&text);
        let _ = text.parse::<Timestamp>();
        let _ = text.parse::<MessageId>();
        let _ = Decimal::new(text);
    }

    #[test]
    fn decimals_agree_with_rust_float_parsing(text in "[+-]?[0-9]{0,6}(\\.[0-9]{0,6})?([eE][+-]?[0-9]{1,3})?") {
        if let Ok(decimal) = Decimal::new(text.clone()) {
            prop_assert!(text.parse::<f64>().is_ok(), "{text}");
            prop_assert_eq!(decimal.as_str(), text.as_str());
        }
    }
}
