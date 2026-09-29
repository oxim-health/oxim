//! The component types available to channel files.

use oxim_core::Registry;
use oxim_devices::DeviceEnvironment;
use oxim_lab::LabEnvironment;
use oxim_script::ScriptEnvironment;
use oxim_transform::TransformEnvironment;

use crate::settings::Settings;

/// The registry plus the shared state that other parts of the program
/// read, such as the device registry for alerts.
pub(crate) struct Components {
    /// Every component type.
    pub(crate) registry: Registry,
    /// The device registry behind `track-device`.
    pub(crate) devices: DeviceEnvironment,
}

/// Builds the registry with every connector, step, normalizer and encoder
/// shipped with OXIM. Code and routing tables are read from the configured
/// tables directory; lab orders, the device registry, the DICOM instance
/// index and the modality worklist are kept in the data directory.
pub(crate) fn registry(settings: &Settings) -> Registry {
    build(settings).registry
}

/// Builds the registry and keeps the shared state (see [`Components`]).
pub(crate) fn build(settings: &Settings) -> Components {
    let mut registry = Registry::new();
    let devices = DeviceEnvironment::new(settings.data_dir.join("devices.db"));
    oxim_connectors::register(&mut registry);
    oxim_connectors_db::register(&mut registry);
    oxim_connectors_messaging::register(&mut registry);
    oxim_connectors_remote::register(&mut registry);
    oxim_mapping::register(&mut registry);
    oxim_fhir::register(&mut registry);
    oxim_dicom::register_with(
        &mut registry,
        &oxim_dicom::DicomEnvironment::new(settings.data_dir.clone()),
    );
    oxim_cda::register(&mut registry);
    oxim_transform::register(
        &mut registry,
        TransformEnvironment::new(settings.tables_dir.clone()),
    );
    oxim_lab::register(
        &mut registry,
        LabEnvironment::new(settings.orders_path(), settings.tables_dir.clone()),
    );
    oxim_script::register(
        &mut registry,
        ScriptEnvironment::new(settings.scripts_dir.clone()),
    );
    oxim_devices::register(&mut registry, devices.clone());
    Components { registry, devices }
}
