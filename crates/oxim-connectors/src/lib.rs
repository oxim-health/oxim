//! Source and destination connectors for OXIM channels.
//!
//! Call [`register`] to make every connector type of this crate available
//! to channel configurations.

pub mod astm;
pub mod poct1a;
pub mod serial;

/// Registers every connector type of this crate.
pub fn register(registry: &mut oxim_core::Registry) {
    astm::register(registry);
    poct1a::register(registry);
}
