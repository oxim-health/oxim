//! The `script` filter, transformer and encoder.

use oxim_core::{
    Document, Encoded, Encoder, EngineError, Filter, MessageContext, StepConfig, StepError,
    Transformer,
};

use crate::environment::ScriptEnvironment;
use crate::runtime::{Returned, RunState, STEP, Script};
use crate::settings::{Kind, ScriptSettings};

fn compile(
    step: &StepConfig,
    environment: &ScriptEnvironment,
    kind: Kind,
) -> Result<Script, EngineError> {
    let settings = ScriptSettings::from_step(step, environment, kind)?;
    Script::new(settings, kind, environment.globals.clone())
}

/// Keeps messages for which the script returns `true`.
///
/// The script sees the message read-only: changes to `vars` and `clinical`
/// are discarded and `msg.set` throws.
#[derive(Debug)]
pub struct ScriptFilter {
    script: Script,
}

impl ScriptFilter {
    /// Compiles the script of a `script` filter.
    pub fn from_step(
        step: &StepConfig,
        environment: &ScriptEnvironment,
    ) -> Result<Self, EngineError> {
        Ok(Self {
            script: compile(step, environment, Kind::Filter)?,
        })
    }
}

impl Filter for ScriptFilter {
    fn accept(&self, context: &MessageContext) -> Result<bool, StepError> {
        let state = RunState::new(context.document.clone(), false, &context.envelope);
        let (_, result) = self
            .script
            .run(state, context.clinical.as_ref(), &context.variables);
        match result?.value {
            Returned::Bool(keep) => Ok(keep),
            _ => Err(StepError::new(
                STEP,
                "the filter script returned no decision",
            )),
        }
    }
}

/// Changes the message: its document through `msg`, the normalized
/// content through `clinical`, the variables through `vars` and the reply
/// through `reply()`.
#[derive(Debug)]
pub struct ScriptTransformer {
    script: Script,
}

impl ScriptTransformer {
    /// Compiles the script of a `script` transformer.
    pub fn from_step(
        step: &StepConfig,
        environment: &ScriptEnvironment,
    ) -> Result<Self, EngineError> {
        Ok(Self {
            script: compile(step, environment, Kind::Transformer)?,
        })
    }
}

impl Transformer for ScriptTransformer {
    fn apply(&self, context: &mut MessageContext) -> Result<(), StepError> {
        let document = std::mem::replace(&mut context.document, Document::Raw(Vec::new()));
        let state = RunState::new(document, true, &context.envelope);
        let (state, result) = self
            .script
            .run(state, context.clinical.as_ref(), &context.variables);
        context.document = state.document;
        let outcome = result?;
        if let Some(clinical) = outcome.clinical {
            context.clinical = clinical;
        }
        context.variables = outcome.variables;
        if let Some(response) = state.response {
            context.response = Some(response);
        }
        Ok(())
    }
}

/// Produces the bytes to send: the string or `Uint8Array` the script
/// returns. The data type is the `data_type` setting, or the data type of
/// the message.
#[derive(Debug)]
pub struct ScriptEncoder {
    script: Script,
}

impl ScriptEncoder {
    /// Compiles the script of a `script` encoder.
    pub fn from_step(
        step: &StepConfig,
        environment: &ScriptEnvironment,
    ) -> Result<Self, EngineError> {
        Ok(Self {
            script: compile(step, environment, Kind::Encoder)?,
        })
    }
}

impl Encoder for ScriptEncoder {
    fn encode(&self, context: &MessageContext) -> Result<Encoded, StepError> {
        let state = RunState::new(context.document.clone(), false, &context.envelope);
        let (_, result) = self
            .script
            .run(state, context.clinical.as_ref(), &context.variables);
        match result?.value {
            Returned::Bytes(data) => Ok(Encoded {
                data_type: self
                    .script
                    .output_type()
                    .unwrap_or_else(|| context.document.data_type()),
                data,
            }),
            _ => Err(StepError::new(STEP, "the encoder script returned no data")),
        }
    }
}
