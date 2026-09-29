//! Source and destination connectors for OXIM.
//!
//! Every connector implements [`oxim_core::SourceConnector`] or
//! [`oxim_core::DestinationConnector`] and is registered under a type name
//! that channel configurations use. Sources acknowledge senders only after
//! [`oxim_core::SourceContext::submit`] stored the message durably.
//!
//! | Type | Source | Destination | Module |
//! |---|---|---|---|
//! | `mllp` | HL7 v2 listener with automatic ACK | HL7 v2 client waiting for ACK | [`mllp`] |
//! | `tcp` | Raw TCP with delimiter, length-prefix or per-connection framing | Same framings | [`tcp`] |
//! | `file` | Directory poller | Atomic file writer | [`file`](mod@file) |
//! | `http` | HTTP/HTTPS listener, optionally answering with the reply ([`http_listener`]) | HTTP/HTTPS requests ([`http`]) | |
//! | `astm-tcp` | ASTM LIS01 over TCP, final ACK after durable storage | ASTM LIS01 over TCP (worklists) | [`astm`] |
//! | `astm-serial` | ASTM LIS01 over RS-232 | ASTM LIS01 over RS-232 | [`astm`] |
//! | `astm-raw-tcp` | ASTM records without LIS01 framing | | [`astm`] |
//! | `poct1a` | POCT1-A device conversations | | [`poct1a`] |
//!
//! `mllp` and `tcp` support TLS and mutual TLS through a `tls` settings
//! block; see [`tls`].

pub mod astm;
pub mod file;
pub mod http;
pub mod http_listener;
pub mod mllp;
mod net;
pub mod poct1a;
pub mod serial;
pub mod tcp;
pub mod tls;

/// Registers every connector of this crate.
pub fn register(registry: &mut oxim_core::Registry) {
    mllp::register(registry);
    tcp::register(registry);
    file::register(registry);
    http::register(registry);
    http_listener::register(registry);
    astm::register(registry);
    poct1a::register(registry);
}
