//! Channel-to-channel routing and the timer source.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::Arc;

use common::{Recorder, engine, wait_until};
use oxim_core::ChannelConfig;
use oxim_model::{ChannelId, MessageStatus};
use oxim_store::MessageQuery;

#[tokio::test(flavor = "multi_thread")]
async fn channels_pass_messages_to_each_other() {
    let recorder = Arc::new(Recorder::default());
    let engine = engine(recorder.clone()).await;
    // The sender first: its deliveries are retried until the receiver is
    // deployed.
    engine
        .deploy(
            ChannelConfig::from_yaml(
                "id: intake
source: {type: timer, data_type: raw, settings: {interval: 50ms, payload: heartbeat, immediately: true}}
destinations:
  - id: forward
    type: channel
    settings: {channel: processing}
    queue: {retry: {initial_delay: 20ms, max_delay: 50ms}}
",
            )
            .unwrap(),
        )
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    assert!(recorder.payloads().is_empty());
    engine
        .deploy(
            ChannelConfig::from_yaml(
                "id: processing
source: {type: channel, data_type: raw}
destinations:
  - {id: out, type: recorder}
",
            )
            .unwrap(),
        )
        .await
        .unwrap();
    wait_until("messages arrive through the channel", || {
        recorder.payloads().len() >= 3
    })
    .await;
    assert!(recorder.payloads().iter().all(|p| p == b"heartbeat"));

    // The received messages point back at the messages that sent them.
    let processing = ChannelId::new("processing").unwrap();
    let records = engine
        .store()
        .run(move |store| {
            store.list_messages(&MessageQuery {
                channel: Some(processing),
                limit: 10,
                ..MessageQuery::default()
            })
        })
        .await
        .unwrap();
    let record = records.first().expect("a received message");
    let origin = record
        .metadata
        .get("channel.message")
        .expect("channel.message metadata")
        .clone();
    assert_eq!(
        record.metadata.get("channel.from").map(String::as_str),
        Some("intake")
    );
    assert_eq!(record.correlation_id.as_deref(), Some(origin.as_str()));
    let origin = origin.parse().unwrap();
    let sent = engine
        .store()
        .run(move |store| store.message(origin))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(sent.status, MessageStatus::Completed);
    engine.shutdown().await;
}

#[test]
fn rejects_bad_settings() {
    let mut registry = oxim_core::Registry::new();
    oxim_connectors::register(&mut registry);
    for source in [
        "{type: timer, data_type: raw, settings: {interval: 1ms}}",
        "{type: timer, data_type: raw, settings: {}}",
        "{type: channel, data_type: raw, settings: {name: x}}",
    ] {
        let config = ChannelConfig::from_yaml(&format!("id: x\nsource: {source}\n")).unwrap();
        assert!(registry.source(&config.source).is_err(), "{source}");
    }
    let config = ChannelConfig::from_yaml(
        "id: x\nsource: {type: timer, data_type: raw, settings: {interval: 1s}}\ndestinations:\n  - {id: d, type: channel, settings: {channel: 'not a valid id'}}\n",
    );
    if let Ok(config) = config {
        assert!(registry.destination(&config.destinations[0]).is_err());
    }
}
