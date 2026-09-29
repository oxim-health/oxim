//! The component types available to channel files.

use oxim_core::Registry;

use crate::settings::Settings;

/// Builds the registry with every connector, step, normalizer and encoder
/// shipped with OXIM.
pub(crate) fn registry(_settings: &Settings) -> Registry {
    Registry::new()
}
