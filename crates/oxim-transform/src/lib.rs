//! Declarative filters, mapping operations and code tables for OXIM
//! channels.
//!
//! [`register`] adds three step types to an [`oxim_core::Registry`]:
//!
//! | Type | Kind | Purpose |
//! |---|---|---|
//! | `condition` | filter | keep messages matching a [`Condition`] tree |
//! | `map` | transformer | edit the document with ordered operations ([`MapTransformer`]) |
//! | `map-observations` | transformer | translate codes in the normalized clinical model ([`MapObservations`]) |
//!
//! ```yaml
//! filters:
//!   - type: condition
//!     all:
//!       - {path: MSH-9.1, in: [ORU, OUL]}
//!       - {path: MSH-3, not_equals: TEST}
//! transformers:
//!   - type: map
//!     operations:
//!       - lookup: {table: tables/tests.csv, from: "OBX[*]-3.1", on_missing: keep}
//!       - set: {path: MSH-5, value: LIS}
//! destinations:
//!   - id: lis
//!     type: mllp
//!     transformers:
//!       - type: map-observations
//!         table: tables/loinc.csv
//!         on_missing: error
//! ```
//!
//! Steps work on values as text and never interpret clinical content (ADR
//! 0011): conditions compare, operations copy and translate, but nothing
//! computes or flags a result. Steps read code tables when a channel is
//! deployed and never touch the network or the clock while processing.

mod condition;
mod date;
mod environment;
mod map;
mod observations;
mod path;
mod settings;
mod table;
mod template;

use std::sync::Arc;

use oxim_core::{Filter, MessageContext, Registry, StepError, Transformer};

pub use condition::Condition;
pub use date::{DateFormat, Field, Token};
pub use environment::TransformEnvironment;
pub use map::MapTransformer;
pub use observations::MapObservations;
pub use table::{CodeEntry, CodeTable, CodeTableError};
pub use template::Template;

/// The `condition` filter: keeps messages for which the condition holds.
#[derive(Debug, Clone)]
pub struct ConditionFilter {
    condition: Condition,
}

impl Filter for ConditionFilter {
    fn accept(&self, context: &MessageContext) -> Result<bool, StepError> {
        self.condition.evaluate(context, None)
    }
}

/// Registers the `condition`, `map` and `map-observations` step types.
/// Code tables are resolved through `environment`.
pub fn register(registry: &mut Registry, environment: TransformEnvironment) {
    let map_environment = environment.clone();
    registry
        .add_filter("condition", |step| {
            let settings = serde_json::Value::Object(step.settings.clone());
            Ok(Arc::new(ConditionFilter {
                condition: Condition::parse(&settings)?,
            }) as Arc<dyn Filter>)
        })
        .add_transformer("map", move |step| {
            Ok(Arc::new(MapTransformer::from_step(step, &map_environment)?)
                as Arc<dyn Transformer>)
        })
        .add_transformer("map-observations", move |step| {
            Ok(Arc::new(MapObservations::from_step(step, &environment)?) as Arc<dyn Transformer>)
        });
}

#[cfg(test)]
pub(crate) mod test_support {
    use oxim_core::{DocumentParser, MessageContext, StepConfig};
    use oxim_model::{ChannelId, ConnectorId, DataType, Envelope, MessageId, Timestamp};

    fn build(data_type: DataType, raw: &[u8]) -> MessageContext {
        let envelope = Envelope::new(
            MessageId::from_parts(1_790_000_000_000, 7),
            ChannelId::new("lab").unwrap(),
            ConnectorId::new("source").unwrap(),
            Timestamp::from_unix_nanos(1_790_000_000_000_000_000),
            data_type,
            raw.to_vec(),
        );
        MessageContext {
            document: DocumentParser::new(data_type, None)
                .unwrap()
                .parse(raw)
                .unwrap(),
            envelope,
            clinical: None,
            variables: std::collections::BTreeMap::new(),
        }
    }

    /// A context holding an HL7 v2 message.
    pub(crate) fn context(raw: &[u8]) -> MessageContext {
        build(DataType::Hl7V2, raw)
    }

    /// A context holding an ASTM message.
    pub(crate) fn astm_context(raw: &[u8]) -> MessageContext {
        build(DataType::Astm, raw)
    }

    /// A step configuration with settings from a JSON object.
    pub(crate) fn step(kind: &str, settings: serde_json::Value) -> StepConfig {
        StepConfig {
            kind: kind.to_owned(),
            settings: match settings {
                serde_json::Value::Object(map) => map,
                _ => serde_json::Map::new(),
            },
        }
    }
}
