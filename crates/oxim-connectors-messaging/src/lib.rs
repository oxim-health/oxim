//! Messaging connectors for OXIM channels.
//!
//! | Type | System | Source | Destination |
//! |---|---|---|---|
//! | `mqtt` | MQTT 3.1.1 brokers | subscription; QoS 1 acknowledged after storage | publish; QoS 1 waits for the broker |
//! | `amqp` | AMQP 0-9-1 (RabbitMQ) | queue consumer; ack after storage | publish with publisher confirms |
//! | `kafka` | Apache Kafka | partition reader with an offsets file | produce with `acks=all` |
//! | `nats` | NATS, JetStream | subscription or durable consumer | publish, or JetStream publish with acknowledgment |
//!
//! Every source acknowledges a message to its broker only after OXIM stored
//! it durably, so a crash never loses a message (it may arrive twice:
//! delivery is at-least-once). Every destination reports success only once
//! the broker confirmed the message. TLS uses rustls with the ring
//! provider and the `tls` settings of [`oxim_connectors::tls`].

pub mod amqp;
mod common;
pub mod kafka;
pub mod mqtt;
pub mod nats;
mod template;

/// Registers every connector of this crate.
pub fn register(registry: &mut oxim_core::Registry) {
    mqtt::register(registry);
    amqp::register(registry);
    kafka::register(registry);
    nats::register(registry);
}
