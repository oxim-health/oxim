//! Filters, transformers, normalizers and encoders, and the function that
//! runs a message through a channel.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use oxim_model::{ClinicalContent, ConnectorId, DataType, Envelope, MessageStatus};
use oxim_store::{Content, Processed, Stage};

use crate::config::QueueConfig;
use crate::document::{Document, DocumentParser};
use crate::error::StepError;

/// Everything a pipeline step can see and change about a message.
#[derive(Debug, Clone)]
pub struct MessageContext {
    /// The message as received.
    pub envelope: Envelope,
    /// The parsed message; transformers may edit it.
    pub document: Document,
    /// The normalized clinical content, when the channel normalizes.
    pub clinical: Option<ClinicalContent>,
    /// Values shared between steps, such as a looked-up code.
    pub variables: BTreeMap<String, String>,
}

/// Decides whether a message continues.
pub trait Filter: Send + Sync + fmt::Debug {
    /// Returns `true` to keep the message.
    fn accept(&self, context: &MessageContext) -> Result<bool, StepError>;
}

/// Changes a message.
pub trait Transformer: Send + Sync + fmt::Debug {
    /// Applies the change.
    fn apply(&self, context: &mut MessageContext) -> Result<(), StepError>;
}

/// Maps a parsed document to the normalized clinical model.
pub trait Normalizer: Send + Sync + fmt::Debug {
    /// Produces the normalized content.
    fn normalize(&self, document: &Document) -> Result<ClinicalContent, StepError>;
}

/// Produces the bytes a destination sends.
pub trait Encoder: Send + Sync + fmt::Debug {
    /// Encodes the message.
    fn encode(&self, context: &MessageContext) -> Result<Encoded, StepError>;
}

/// Bytes produced by an [`Encoder`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Encoded {
    /// The data type of `data`.
    pub data_type: DataType,
    /// The bytes to send.
    pub data: Vec<u8>,
}

/// Sends the (possibly transformed) document unchanged.
#[derive(Debug, Clone, Copy, Default)]
pub struct PassthroughEncoder;

impl Encoder for PassthroughEncoder {
    fn encode(&self, context: &MessageContext) -> Result<Encoded, StepError> {
        Ok(Encoded {
            data_type: context.document.data_type(),
            data: context.document.to_bytes(),
        })
    }
}

/// A destination ready to process messages.
#[derive(Debug, Clone)]
pub struct CompiledDestination {
    /// Destination identifier.
    pub id: ConnectorId,
    /// Destination filters.
    pub filters: Vec<Arc<dyn Filter>>,
    /// Destination transformers.
    pub transformers: Vec<Arc<dyn Transformer>>,
    /// The encoder.
    pub encoder: Arc<dyn Encoder>,
    /// Queue behavior.
    pub queue: QueueConfig,
}

/// A channel's processing steps, ready to run.
#[derive(Debug, Clone)]
pub struct CompiledPipeline {
    /// Parses the raw bytes.
    pub parser: DocumentParser,
    /// Maps to the normalized model, when enabled.
    pub normalizer: Option<Arc<dyn Normalizer>>,
    /// Channel filters.
    pub filters: Vec<Arc<dyn Filter>>,
    /// Channel transformers.
    pub transformers: Vec<Arc<dyn Transformer>>,
    /// Destinations in configuration order.
    pub destinations: Vec<CompiledDestination>,
}

fn errored(error: StepError) -> Processed {
    Processed {
        status: MessageStatus::Error,
        error: Some(error.to_string()),
        contents: Vec::new(),
        queue: Vec::new(),
        filtered: Vec::new(),
    }
}

fn run_filters(
    filters: &[Arc<dyn Filter>],
    context: &MessageContext,
    scope: &str,
) -> Result<bool, StepError> {
    for (index, filter) in filters.iter().enumerate() {
        let accepted = filter
            .accept(context)
            .map_err(|e| StepError::new(format!("{scope}filter[{index}]"), e.to_string()))?;
        if !accepted {
            return Ok(false);
        }
    }
    Ok(true)
}

fn run_transformers(
    transformers: &[Arc<dyn Transformer>],
    context: &mut MessageContext,
    scope: &str,
) -> Result<(), StepError> {
    for (index, transformer) in transformers.iter().enumerate() {
        transformer
            .apply(context)
            .map_err(|e| StepError::new(format!("{scope}transformer[{index}]"), e.to_string()))?;
    }
    Ok(())
}

impl CompiledPipeline {
    /// Runs `envelope` through the pipeline and returns what the store
    /// records: the final status, the contents of every stage and the
    /// destinations to queue. Step failures produce status `Error` so the
    /// message can be reprocessed after the cause is fixed; they never
    /// panic or lose the message.
    pub fn process(&self, envelope: Envelope) -> Processed {
        match self.try_process(envelope) {
            Ok(processed) => processed,
            Err(error) => errored(error),
        }
    }

    fn try_process(&self, envelope: Envelope) -> Result<Processed, StepError> {
        let document = self.parser.parse(&envelope.raw)?;
        let clinical = match &self.normalizer {
            Some(normalizer) => Some(
                normalizer
                    .normalize(&document)
                    .map_err(|e| StepError::new("normalize", e.to_string()))?,
            ),
            None => None,
        };
        let mut context = MessageContext {
            envelope,
            document,
            clinical,
            variables: BTreeMap::new(),
        };

        let mut contents = Vec::new();
        if !run_filters(&self.filters, &context, "")? {
            return Ok(Processed {
                status: MessageStatus::Filtered,
                error: None,
                contents,
                queue: Vec::new(),
                filtered: Vec::new(),
            });
        }
        run_transformers(&self.transformers, &mut context, "")?;

        if let Some(clinical) = &context.clinical {
            let json = serde_json::to_vec(clinical)
                .map_err(|e| StepError::new("normalize", e.to_string()))?;
            contents.push(Content::new(Stage::Normalized, Some(DataType::Json), json));
        }
        contents.push(Content::new(
            Stage::Transformed,
            Some(context.document.data_type()),
            context.document.to_bytes(),
        ));

        let mut queue = Vec::new();
        let mut filtered = Vec::new();
        for destination in &self.destinations {
            let scope = format!("{}/", destination.id);
            if !run_filters(&destination.filters, &context, &scope)? {
                filtered.push(destination.id.clone());
                continue;
            }
            let encoded = if destination.transformers.is_empty() {
                destination.encoder.encode(&context)
            } else {
                let mut own = context.clone();
                run_transformers(&destination.transformers, &mut own, &scope)?;
                destination.encoder.encode(&own)
            }
            .map_err(|e| StepError::new(format!("{scope}encoder"), e.to_string()))?;
            contents.push(Content::for_destination(
                Stage::Encoded,
                destination.id.clone(),
                Some(encoded.data_type),
                encoded.data,
            ));
            queue.push(destination.id.clone());
        }

        Ok(Processed {
            status: MessageStatus::Transformed,
            error: None,
            contents,
            queue,
            filtered,
        })
    }
}
