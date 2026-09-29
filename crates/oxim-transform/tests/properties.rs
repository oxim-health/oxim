//! Property tests: parsing and evaluating arbitrary templates, conditions,
//! date formats and code tables never panics.

#![allow(clippy::unwrap_used)]

use oxim_core::{DocumentParser, MessageContext};
use oxim_model::{ChannelId, ConnectorId, DataType, Envelope, MessageId, Timestamp};
use oxim_transform::{CodeTable, Condition, DateFormat, Template};
use proptest::prelude::*;
use serde_json::json;

fn context() -> MessageContext {
    let raw = b"MSH|^~\\&|LAB|HOSP|||20260929||ORU^R01|1|P|2.5\rPID|1||42||Doe^Jane\rOBX|1|NM|GLU||5.4\rOBX|2|ST|NOTE||<0.5\r";
    MessageContext {
        document: DocumentParser::new(DataType::Hl7V2, None)
            .unwrap()
            .parse(raw)
            .unwrap(),
        envelope: Envelope::new(
            MessageId::from_parts(1, 1),
            ChannelId::new("lab").unwrap(),
            ConnectorId::new("source").unwrap(),
            Timestamp::from_unix_nanos(0),
            DataType::Hl7V2,
            raw.to_vec(),
        ),
        clinical: None,
        variables: [("x".to_owned(), "1".to_owned())].into(),
        response: None,
    }
}

fn text() -> impl Strategy<Value = String> {
    prop_oneof![
        "\\PC{0,30}",
        "[{}$|\\[\\]*A-Z0-9.\\-]{0,20}",
        Just("OBX[*]-5".to_owned()),
        Just("{PID-5.1|x}{$x}{{}}".to_owned()),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    #[test]
    fn templates_never_panic(template in text()) {
        let ctx = context();
        if let Ok(template) = Template::parse(&template) {
            let _ = template.render(&ctx, None);
            let _ = template.render(&ctx, Some(2));
        }
    }

    #[test]
    fn conditions_never_panic(path in text(), value in text(), test in 0usize..8, insensitive in any::<bool>()) {
        let ctx = context();
        let key = ["equals", "not_equals", "matches", "greater_than", "less_or_equal", "in", "exists", "empty"][test];
        let operand = match key {
            "in" => json!([value]),
            "exists" | "empty" => json!(insensitive),
            _ => json!(value),
        };
        let condition = json!({"path": path, key: operand, "case_insensitive": insensitive});
        if let Ok(condition) = Condition::parse(&condition) {
            let _ = condition.evaluate(&ctx, None);
            let _ = condition.evaluate(&ctx, Some(1));
        }
        let comparison = json!({"path": "OBX[*]-5", "greater_than": value});
        if let Ok(condition) = Condition::parse(&comparison) {
            let _ = condition.evaluate(&ctx, None);
        }
    }

    #[test]
    fn dates_and_tables_never_panic(pattern in "[%YmdHMS.:/ \\-]{0,16}", value in "\\PC{0,20}", csv in "[a-z,\"\\r\\n ]{0,60}") {
        if let Ok(format) = DateFormat::parse("test", &pattern)
            && let Ok(date) = format.read(&value)
        {
            let _ = format.write(&date);
        }
        let _ = DateFormat::parse("test", "hl7").unwrap().read(&value);
        let _ = CodeTable::from_csv(&format!("from,to\n{csv}"));
        let _ = CodeTable::from_csv(&csv);
    }
}
