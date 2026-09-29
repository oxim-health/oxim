//! Named connector and step types available to channel configurations.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use oxim_model::DataType;

use crate::config::{ChannelConfig, DestinationConfig, SourceConfig, StepConfig};
use crate::connector::{DestinationConnector, SourceConnector};
use crate::document::DocumentParser;
use crate::error::EngineError;
use crate::pipeline::{
    CompiledDestination, CompiledPipeline, Encoder, Filter, Normalizer, PassthroughEncoder,
    Transformer,
};
use crate::steps;

type Factory<C, T> = Arc<dyn Fn(&C) -> Result<T, EngineError> + Send + Sync>;

/// The component types a channel configuration can name.
///
/// [`Registry::new`] contains the built-in steps of this crate (see
/// [`crate::steps`]); connector crates, the transformation crate and
/// plugins add their types with the `add_*` methods.
#[derive(Clone)]
pub struct Registry {
    sources: BTreeMap<String, Factory<SourceConfig, Arc<dyn SourceConnector>>>,
    destinations: BTreeMap<String, Factory<DestinationConfig, Arc<dyn DestinationConnector>>>,
    filters: BTreeMap<String, Factory<StepConfig, Arc<dyn Filter>>>,
    transformers: BTreeMap<String, Factory<StepConfig, Arc<dyn Transformer>>>,
    encoders: BTreeMap<String, Factory<StepConfig, Arc<dyn Encoder>>>,
    normalizers: BTreeMap<DataType, Arc<dyn Normalizer>>,
}

impl fmt::Debug for Registry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Registry")
            .field("sources", &self.sources.keys().collect::<Vec<_>>())
            .field(
                "destinations",
                &self.destinations.keys().collect::<Vec<_>>(),
            )
            .field("filters", &self.filters.keys().collect::<Vec<_>>())
            .field(
                "transformers",
                &self.transformers.keys().collect::<Vec<_>>(),
            )
            .field("encoders", &self.encoders.keys().collect::<Vec<_>>())
            .field("normalizers", &self.normalizers.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

impl Registry {
    /// A registry with the built-in steps and no connectors.
    pub fn new() -> Self {
        let mut registry = Self::empty();
        steps::register(&mut registry);
        registry
    }

    /// A registry without any component.
    pub fn empty() -> Self {
        Self {
            sources: BTreeMap::new(),
            destinations: BTreeMap::new(),
            filters: BTreeMap::new(),
            transformers: BTreeMap::new(),
            encoders: BTreeMap::new(),
            normalizers: BTreeMap::new(),
        }
    }

    /// Registers a source connector type.
    pub fn add_source(
        &mut self,
        name: &str,
        factory: impl Fn(&SourceConfig) -> Result<Arc<dyn SourceConnector>, EngineError>
        + Send
        + Sync
        + 'static,
    ) -> &mut Self {
        self.sources.insert(name.to_owned(), Arc::new(factory));
        self
    }

    /// Registers a destination connector type.
    pub fn add_destination(
        &mut self,
        name: &str,
        factory: impl Fn(&DestinationConfig) -> Result<Arc<dyn DestinationConnector>, EngineError>
        + Send
        + Sync
        + 'static,
    ) -> &mut Self {
        self.destinations.insert(name.to_owned(), Arc::new(factory));
        self
    }

    /// Registers a filter type.
    pub fn add_filter(
        &mut self,
        name: &str,
        factory: impl Fn(&StepConfig) -> Result<Arc<dyn Filter>, EngineError> + Send + Sync + 'static,
    ) -> &mut Self {
        self.filters.insert(name.to_owned(), Arc::new(factory));
        self
    }

    /// Registers a transformer type.
    pub fn add_transformer(
        &mut self,
        name: &str,
        factory: impl Fn(&StepConfig) -> Result<Arc<dyn Transformer>, EngineError>
        + Send
        + Sync
        + 'static,
    ) -> &mut Self {
        self.transformers.insert(name.to_owned(), Arc::new(factory));
        self
    }

    /// Registers an encoder type.
    pub fn add_encoder(
        &mut self,
        name: &str,
        factory: impl Fn(&StepConfig) -> Result<Arc<dyn Encoder>, EngineError> + Send + Sync + 'static,
    ) -> &mut Self {
        self.encoders.insert(name.to_owned(), Arc::new(factory));
        self
    }

    /// Registers the normalizer used for a data type when a channel sets
    /// `normalize: true`.
    pub fn add_normalizer(
        &mut self,
        data_type: DataType,
        normalizer: Arc<dyn Normalizer>,
    ) -> &mut Self {
        self.normalizers.insert(data_type, normalizer);
        self
    }

    /// Names of the registered types, for diagnostics and the UI.
    pub fn type_names(&self) -> BTreeMap<&'static str, Vec<String>> {
        BTreeMap::from([
            ("source", self.sources.keys().cloned().collect()),
            ("destination", self.destinations.keys().cloned().collect()),
            ("filter", self.filters.keys().cloned().collect()),
            ("transformer", self.transformers.keys().cloned().collect()),
            ("encoder", self.encoders.keys().cloned().collect()),
        ])
    }

    fn build<C, T>(
        map: &BTreeMap<String, Factory<C, T>>,
        kind: &'static str,
        name: &str,
        config: &C,
    ) -> Result<T, EngineError> {
        let factory = map.get(name).ok_or_else(|| EngineError::UnknownType {
            kind,
            name: name.to_owned(),
        })?;
        factory(config)
    }

    /// Builds the source connector of a channel.
    pub fn source(&self, config: &SourceConfig) -> Result<Arc<dyn SourceConnector>, EngineError> {
        Self::build(&self.sources, "source connector", &config.kind, config)
    }

    /// Builds a destination connector.
    pub fn destination(
        &self,
        config: &DestinationConfig,
    ) -> Result<Arc<dyn DestinationConnector>, EngineError> {
        Self::build(
            &self.destinations,
            "destination connector",
            &config.kind,
            config,
        )
    }

    fn filters(&self, steps: &[StepConfig]) -> Result<Vec<Arc<dyn Filter>>, EngineError> {
        steps
            .iter()
            .map(|step| Self::build(&self.filters, "filter", &step.kind, step))
            .collect()
    }

    fn transformers(&self, steps: &[StepConfig]) -> Result<Vec<Arc<dyn Transformer>>, EngineError> {
        steps
            .iter()
            .map(|step| Self::build(&self.transformers, "transformer", &step.kind, step))
            .collect()
    }

    /// Builds the processing pipeline of a channel.
    pub fn compile(&self, channel: &ChannelConfig) -> Result<CompiledPipeline, EngineError> {
        channel.validate()?;
        let with_channel =
            |e: EngineError| EngineError::Config(format!("channel {}: {e}", channel.id));
        let normalizer = if channel.source.normalize {
            Some(
                self.normalizers
                    .get(&channel.source.data_type)
                    .cloned()
                    .ok_or_else(|| {
                        with_channel(EngineError::Config(format!(
                            "no normalizer is available for {}",
                            channel.source.data_type
                        )))
                    })?,
            )
        } else {
            None
        };
        let destinations = channel
            .destinations
            .iter()
            .map(|destination| {
                let encoder = match &destination.encoder {
                    Some(step) => Self::build(&self.encoders, "encoder", &step.kind, step)?,
                    None => Arc::new(PassthroughEncoder) as Arc<dyn Encoder>,
                };
                Ok(CompiledDestination {
                    id: destination.id.clone(),
                    filters: self.filters(&destination.filters)?,
                    transformers: self.transformers(&destination.transformers)?,
                    encoder,
                    queue: destination.queue,
                })
            })
            .collect::<Result<Vec<_>, EngineError>>()
            .map_err(with_channel)?;
        Ok(CompiledPipeline {
            parser: DocumentParser::new(channel.source.data_type, channel.source.format.as_ref())
                .map_err(with_channel)?,
            normalizer,
            filters: self.filters(&channel.filters).map_err(with_channel)?,
            transformers: self
                .transformers(&channel.transformers)
                .map_err(with_channel)?,
            destinations,
        })
    }
}
