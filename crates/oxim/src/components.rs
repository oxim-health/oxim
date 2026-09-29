//! The component types available to channel files.

use oxim_core::Registry;
use oxim_lab::LabEnvironment;
use oxim_transform::TransformEnvironment;

use crate::settings::Settings;

/// Builds the registry with every connector, step, normalizer and encoder
/// shipped with OXIM. Code and routing tables are read from the configured
/// tables directory; lab orders are cached in the data directory.
pub(crate) fn registry(settings: &Settings) -> Registry {
    let mut registry = Registry::new();
    oxim_connectors::register(&mut registry);
    oxim_mapping::register(&mut registry);
    oxim_transform::register(
        &mut registry,
        TransformEnvironment::new(settings.tables_dir.clone()),
    );
    oxim_lab::register(
        &mut registry,
        LabEnvironment::new(settings.orders_path(), settings.tables_dir.clone()),
    );
    registry
}
