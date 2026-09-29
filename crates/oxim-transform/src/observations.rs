//! The `map-observations` transformer: code translation on the normalized
//! clinical model.

use std::sync::Arc;

use oxim_core::{EngineError, MessageContext, StepConfig, StepError, Transformer};
use oxim_model::{ClinicalContent, CodeableConcept, Coding, Observation};

use crate::environment::TransformEnvironment;
use crate::settings::{Obj, config_error};
use crate::table::CodeTable;
use crate::template::Template;

/// What happens to a code that is not in the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OnMissing {
    Keep,
    Drop,
    Error,
}

/// Translates observation and test codes of the normalized clinical content
/// through a code table.
///
/// ```yaml
/// transformers:
///   - type: map-observations
///     table: tables/chemistry.csv
///     system: http://loinc.org      # optional; otherwise the table's system column
///     context: "{message.device}"   # optional; selects context rows of the table
///     on_missing: keep              # keep | drop | error
///     case_insensitive: false
///     device_id: "{message.device}" # optional; written to every observation
///     operator: "{$operator}"       # optional; written to every observation
/// ```
///
/// It applies to result observations, quality-control observations, order
/// tests and host-query tests. A translated code becomes the primary coding
/// and the original device coding is kept after it, so nothing is lost.
/// Values, units, reference ranges and flags are never touched: this step
/// renames what was measured, it does not interpret the measurement.
///
/// With `on_missing: drop`, unmapped observations and tests are removed;
/// with `error`, the message is marked as errored and can be reprocessed
/// after the table is completed. The channel must set `normalize: true`.
#[derive(Debug, Clone)]
pub struct MapObservations {
    table: Arc<CodeTable>,
    system: Option<String>,
    context: Option<Template>,
    on_missing: OnMissing,
    device_id: Option<Template>,
    operator: Option<Template>,
}

impl MapObservations {
    /// Builds the transformer from its step configuration.
    pub fn from_step(
        step: &StepConfig,
        environment: &TransformEnvironment,
    ) -> Result<Self, EngineError> {
        let at = "map-observations";
        let obj = Obj::from_map(at, &step.settings);
        obj.only(&[
            "table",
            "system",
            "context",
            "on_missing",
            "case_insensitive",
            "device_id",
            "operator",
        ])?;
        let template = |key: &str| -> Result<Option<Template>, EngineError> {
            obj.text(key)?
                .map(|text| Template::parse_at(at, &text))
                .transpose()
        };
        Ok(Self {
            table: environment.table(
                &obj.required_text("table")?,
                obj.bool("case_insensitive")?.unwrap_or(false),
            )?,
            system: obj.text("system")?,
            context: template("context")?,
            on_missing: match obj.text("on_missing")?.as_deref() {
                None | Some("keep") => OnMissing::Keep,
                Some("drop") => OnMissing::Drop,
                Some("error") => OnMissing::Error,
                Some(other) => {
                    return Err(config_error(
                        at,
                        format!("on_missing must be keep, drop or error, not {other:?}"),
                    ));
                }
            },
            device_id: template("device_id")?,
            operator: template("operator")?,
        })
    }

    /// Translates one concept. Returns `false` when the concept must be
    /// dropped.
    fn translate(
        &self,
        concept: &mut CodeableConcept,
        scope: Option<&str>,
    ) -> Result<bool, StepError> {
        let Some(code) = concept.primary_code().map(str::to_owned) else {
            return Ok(true);
        };
        match self.table.lookup(scope, &code) {
            Some(entry) => {
                concept.codings.insert(
                    0,
                    Coding {
                        system: self.system.clone().or_else(|| entry.system.clone()),
                        code: entry.to.clone(),
                        display: entry.display.clone(),
                    },
                );
                Ok(true)
            }
            None => match self.on_missing {
                OnMissing::Keep => Ok(true),
                OnMissing::Drop => Ok(false),
                OnMissing::Error => Err(StepError::new(
                    "map-observations",
                    format!("code {code:?} is not in the table"),
                )),
            },
        }
    }

    fn observations(
        &self,
        observations: &mut Vec<Observation>,
        scope: Option<&str>,
        device_id: Option<&str>,
        operator: Option<&str>,
    ) -> Result<(), StepError> {
        let mut kept = Vec::with_capacity(observations.len());
        for mut observation in observations.drain(..) {
            if !self.translate(&mut observation.code, scope)? {
                continue;
            }
            if let Some(device_id) = device_id {
                observation.device_id = Some(device_id.to_owned());
            }
            if let Some(operator) = operator {
                observation.operator = Some(operator.to_owned());
            }
            kept.push(observation);
        }
        *observations = kept;
        Ok(())
    }

    fn tests(
        &self,
        tests: &mut Vec<CodeableConcept>,
        scope: Option<&str>,
    ) -> Result<(), StepError> {
        let mut kept = Vec::with_capacity(tests.len());
        for mut test in tests.drain(..) {
            if self.translate(&mut test, scope)? {
                kept.push(test);
            }
        }
        *tests = kept;
        Ok(())
    }
}

impl Transformer for MapObservations {
    fn apply(&self, context: &mut MessageContext) -> Result<(), StepError> {
        let render = |template: &Option<Template>| -> Result<Option<String>, StepError> {
            Ok(template
                .as_ref()
                .map(|t| t.render(context, None))
                .transpose()?
                .filter(|text| !text.is_empty()))
        };
        let scope = render(&self.context)?;
        let device_id = render(&self.device_id)?;
        let operator = render(&self.operator)?;
        let Some(content) = context.clinical.as_mut() else {
            return Err(StepError::new(
                "map-observations",
                "no normalized content; set `normalize: true` on the channel source",
            ));
        };
        let (scope, device_id, operator) =
            (scope.as_deref(), device_id.as_deref(), operator.as_deref());
        match content {
            ClinicalContent::Results { groups, .. } => {
                for group in groups {
                    self.observations(&mut group.observations, scope, device_id, operator)?;
                }
            }
            ClinicalContent::QualityControl { results, .. } => {
                let mut kept = Vec::with_capacity(results.len());
                for mut result in results.drain(..) {
                    let mut single = vec![result.observation];
                    self.observations(&mut single, scope, device_id, operator)?;
                    if let Some(observation) = single.pop() {
                        result.observation = observation;
                        kept.push(result);
                    }
                }
                *results = kept;
            }
            ClinicalContent::Orders { groups } => {
                for group in groups {
                    self.tests(&mut group.order.tests, scope)?;
                }
            }
            ClinicalContent::Query { query, .. } => self.tests(&mut query.tests, scope)?,
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use oxim_model::{
        Decimal, ObservationValue, Order, OrderGroup, QcResult, Quantity, ResultGroup,
        SpecimenQuery,
    };
    use serde_json::json;

    use crate::test_support::{context, step};

    use super::*;

    fn observation(code: &str, value: &str) -> Observation {
        Observation {
            code: CodeableConcept::from_coding(Coding::new(code).with_system("urn:device")),
            value: Some(ObservationValue::Quantity(Quantity::new(
                Decimal::new(value).unwrap(),
                Some("mmol/L".into()),
            ))),
            ..Observation::default()
        }
    }

    fn transformer(settings: serde_json::Value) -> Result<MapObservations, EngineError> {
        let environment = TransformEnvironment::in_memory().with_table(
            "chem",
            CodeTable::from_csv("from,to,display,system,context\nGLU,2345-7,Glucose,http://loinc.org,\nGLU,2339-0,Glucose (POCT),http://loinc.org,poct\n").unwrap(),
        );
        MapObservations::from_step(&step("map-observations", settings), &environment)
    }

    fn results(content: &ClinicalContent) -> &Vec<Observation> {
        match content {
            ClinicalContent::Results { groups, .. } => &groups[0].observations,
            _ => panic!("expected results"),
        }
    }

    #[test]
    fn translates_codes_and_keeps_values() {
        let step = transformer(json!({"table": "chem", "operator": "{$tech}", "device_id": "{message.metadata.device}"})).unwrap();
        let mut ctx = context(b"MSH|^~\\&\r");
        ctx.variables.insert("tech".into(), "ayse".into());
        ctx.envelope
            .metadata
            .insert("device".into(), "chem-1".into());
        ctx.clinical = Some(ClinicalContent::Results {
            device: None,
            groups: vec![ResultGroup {
                observations: vec![observation("GLU", "5.40"), observation("NA", "140")],
                ..ResultGroup::default()
            }],
        });
        let before = ctx.clinical.clone();
        step.apply(&mut ctx).unwrap();
        let observations = results(ctx.clinical.as_ref().unwrap());
        assert_eq!(observations.len(), 2);
        let glucose = &observations[0];
        assert_eq!(glucose.code.primary_code(), Some("2345-7"));
        assert_eq!(
            glucose.code.codings[0].system.as_deref(),
            Some("http://loinc.org")
        );
        assert_eq!(glucose.code.codings[1].code, "GLU");
        assert_eq!(glucose.value, results(before.as_ref().unwrap())[0].value);
        assert_eq!(glucose.operator.as_deref(), Some("ayse"));
        assert_eq!(glucose.device_id.as_deref(), Some("chem-1"));
        assert_eq!(observations[1].code.primary_code(), Some("NA"));
    }

    #[test]
    fn drops_or_rejects_unmapped_codes() {
        let content = ClinicalContent::QualityControl {
            device: None,
            results: vec![
                QcResult {
                    observation: observation("GLU", "5"),
                    ..QcResult::default()
                },
                QcResult {
                    observation: observation("NA", "140"),
                    ..QcResult::default()
                },
            ],
        };
        let mut ctx = context(b"MSH|^~\\&\r");
        ctx.clinical = Some(content.clone());
        transformer(json!({"table": "chem", "on_missing": "drop", "context": "poct"}))
            .unwrap()
            .apply(&mut ctx)
            .unwrap();
        match ctx.clinical.as_ref().unwrap() {
            ClinicalContent::QualityControl { results, .. } => {
                assert_eq!(results.len(), 1);
                assert_eq!(results[0].observation.code.primary_code(), Some("2339-0"));
            }
            _ => panic!("expected QC"),
        }
        ctx.clinical = Some(content);
        let error = transformer(json!({"table": "chem", "on_missing": "error"}))
            .unwrap()
            .apply(&mut ctx)
            .unwrap_err();
        assert!(error.message.contains("NA"));
    }

    #[test]
    fn translates_order_and_query_tests() {
        let step = transformer(json!({"table": "chem", "system": "urn:lis", "on_missing": "drop"}))
            .unwrap();
        let mut ctx = context(b"MSH|^~\\&\r");
        ctx.clinical = Some(ClinicalContent::Orders {
            groups: vec![OrderGroup {
                order: Order {
                    tests: vec![
                        CodeableConcept::from_coding(Coding::new("GLU")),
                        CodeableConcept::from_coding(Coding::new("ZZ")),
                    ],
                    ..Order::default()
                },
                ..OrderGroup::default()
            }],
        });
        step.apply(&mut ctx).unwrap();
        match ctx.clinical.as_ref().unwrap() {
            ClinicalContent::Orders { groups } => {
                assert_eq!(groups[0].order.tests.len(), 1);
                assert_eq!(
                    groups[0].order.tests[0].codings[0].system.as_deref(),
                    Some("urn:lis")
                );
            }
            _ => panic!("expected orders"),
        }
        ctx.clinical = Some(ClinicalContent::Query {
            device: None,
            query: SpecimenQuery {
                tests: vec![CodeableConcept::from_coding(Coding::new("GLU"))],
                ..SpecimenQuery::default()
            },
        });
        step.apply(&mut ctx).unwrap();
        let no_content = &mut context(b"MSH|^~\\&\r");
        assert!(step.apply(no_content).is_err());
    }

    #[test]
    fn validates_settings() {
        assert!(transformer(json!({})).is_err());
        assert!(transformer(json!({"table": "chem", "on_missing": "skip"})).is_err());
        assert!(transformer(json!({"table": "chem", "extra": 1})).is_err());
        assert!(transformer(json!({"table": "chem", "context": "{"})).is_err());
    }
}
