//! The device connectivity toolkit of OXIM: device profiles, the device
//! registry and profile conformance tests.
//!
//! - [`profile`]: the versioned YAML format that describes a device model:
//!   transport and protocol dialect, the OXIM channel that talks to it,
//!   default code tables, known quirks, a setup guide, a verification level
//!   with evidence, and fixtures (recorded sessions with expected outcomes).
//! - [`registry`]: which devices sent messages, when, and how they
//!   identified themselves; which ones fell silent.
//! - The `track-device` step ([`TrackDevice`]) feeds the registry from
//!   channels.
//! - [`conformance`]: `oxim profile test`, which replays a profile's
//!   fixtures through its channel and reports what matched.
//!
//! ```yaml
//! transformers:
//!   - {type: track-device, device: chem-1, silence_after: 30m}
//! ```
//!
//! Recordings for fixtures come from `oxim-capture`; `oxim-anonymize`
//! removes protected health information before they are shared.

pub mod conformance;
pub mod profile;
pub mod registry;
mod steps;

use std::sync::Arc;

use oxim_core::{Registry, Transformer};

pub use conformance::{ConformanceReport, FixtureResult, TestOptions, test_profile};
pub use profile::{LoadedProfile, Profile, ProfileError, VerificationLevel};
pub use registry::{
    Declaration, DeviceIdentity, DeviceRegistry, DeviceState, DeviceStatus, RegistryError,
    RegistryResult,
};
pub use steps::{DeviceEnvironment, TrackDevice, identity};

/// Registers the `track-device` step with an engine registry.
pub fn register(registry: &mut Registry, environment: DeviceEnvironment) {
    registry.add_transformer("track-device", move |step| {
        Ok(Arc::new(TrackDevice::from_step(step, &environment)?) as Arc<dyn Transformer>)
    });
}
