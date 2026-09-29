//! The PostgreSQL store against a real server. Set `OXIM_TEST_POSTGRES_URL`
//! (for example `host=127.0.0.1 port=5432 user=postgres dbname=oxim_test`)
//! to run; each test works in its own schema. Without it the tests pass
//! without checking anything.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use oxim_model::{
    ChannelId, ConnectorId, DataType, DestinationStatus, Envelope, MessageId, MessageStatus,
    Timestamp,
};
use oxim_store::{
    AuditEvent, Content, DeliveryOutcome, MessageQuery, MessageStore, Processed, PrunePolicy,
    QueueOrdering, Stage,
};
use oxim_store_postgres::PostgresStore;

/// A store in a fresh schema of the test database, or `None` when no
/// database is configured.
fn store(schema: &str, node: &str) -> Option<PostgresStore> {
    let url = std::env::var("OXIM_TEST_POSTGRES_URL").ok()?;
    let mut admin = postgres::Client::connect(&url, postgres::NoTls).unwrap();
    admin
        .batch_execute(&format!("CREATE SCHEMA IF NOT EXISTS {schema}"))
        .unwrap();
    Some(
        PostgresStore::connect(&format!("{url} options=-csearch_path={schema}"), None, node)
            .unwrap(),
    )
}

fn fresh(schema: &str) {
    if let Ok(url) = std::env::var("OXIM_TEST_POSTGRES_URL") {
        let mut admin = postgres::Client::connect(&url, postgres::NoTls).unwrap();
        admin
            .batch_execute(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE"))
            .unwrap();
    }
}

fn at(seconds: i64) -> Timestamp {
    Timestamp::from_unix_nanos(seconds * 1_000_000_000)
}

fn envelope(n: u64) -> Envelope {
    Envelope::new(
        MessageId::from_parts(1_790_000_000_000 + n, u128::from(n)),
        ChannelId::new("lab").unwrap(),
        ConnectorId::new("source").unwrap(),
        at(i64::try_from(n).unwrap()),
        DataType::Hl7V2,
        format!("MSH|^~\\&|LAB|||||||M{n}|P|2.5.1\r").into_bytes(),
    )
}

fn lis() -> ConnectorId {
    ConnectorId::new("lis").unwrap()
}

fn transformed(payload: &[u8]) -> Processed {
    Processed {
        status: MessageStatus::Transformed,
        error: None,
        contents: vec![Content::for_destination(
            Stage::Encoded,
            lis(),
            Some(DataType::Hl7V2),
            payload.to_vec(),
        )],
        queue: vec![lis()],
        filtered: Vec::new(),
    }
}

#[test]
fn stores_processes_and_delivers_messages() {
    fresh("oxim_t_flow");
    let Some(mut store) = store("oxim_t_flow", "node-a") else {
        return;
    };
    let channel = ChannelId::new("lab").unwrap();
    let messages: Vec<Envelope> = (1..=3).map(envelope).collect();
    store.receive(&messages).unwrap();
    for message in &messages {
        store
            .finish_processing(message.id, &transformed(&message.raw), at(10))
            .unwrap();
    }
    let stats = store.queue_stats(&channel, &lis()).unwrap();
    assert_eq!(stats.queued, 3);
    assert_eq!(stats.oldest_pending_at, Some(at(1)));

    let first = store
        .next_delivery(&channel, &lis(), QueueOrdering::Strict, at(11))
        .unwrap()
        .unwrap();
    assert_eq!(first.message_id, messages[0].id);
    // Strict order: nothing else while the head is being sent.
    assert!(
        store
            .next_delivery(&channel, &lis(), QueueOrdering::Strict, at(11))
            .unwrap()
            .is_none()
    );
    store
        .complete_delivery(
            first.message_id,
            &lis(),
            &DeliveryOutcome::Sent {
                response: Some(b"MSA|AA".to_vec()),
            },
            at(12),
        )
        .unwrap();
    let record = store.message(first.message_id).unwrap().unwrap();
    assert_eq!(record.status, MessageStatus::Completed);
    assert_eq!(record.destinations[0].status, DestinationStatus::Sent);
    let response = store
        .content(first.message_id, Stage::Response, Some(&lis()))
        .unwrap()
        .unwrap();
    assert_eq!(response.data, b"MSA|AA");

    let second = store
        .next_delivery(&channel, &lis(), QueueOrdering::Strict, at(13))
        .unwrap()
        .unwrap();
    store
        .complete_delivery(
            second.message_id,
            &lis(),
            &DeliveryOutcome::Failed {
                error: "rejected".into(),
            },
            at(14),
        )
        .unwrap();
    store.requeue(second.message_id, &lis(), at(15)).unwrap();
    assert_eq!(store.queue_stats(&channel, &lis()).unwrap().queued, 2);

    let listed = store
        .list_messages(&MessageQuery {
            channel: Some(channel.clone()),
            status: Some(MessageStatus::Completed),
            limit: 10,
            ..MessageQuery::default()
        })
        .unwrap();
    assert_eq!(listed.len(), 1);
    store
        .record_content(
            first.message_id,
            &Content::new(Stage::Reply, Some(DataType::Raw), b"reply".to_vec()),
        )
        .unwrap();
    store
        .record_audit(&AuditEvent {
            at: at(16),
            action: "view".into(),
            actor: "admin".into(),
            message_id: Some(first.message_id),
            channel: Some(channel.clone()),
            detail: None,
        })
        .unwrap();
    assert_eq!(
        store.audit_trail(Some(first.message_id), 10).unwrap().len(),
        1
    );
    let counts = store.status_counts().unwrap();
    assert!(!counts.messages.is_empty());
    let pruned = store
        .prune(&PrunePolicy {
            contents_before: None,
            messages_before: Some(at(100)),
        })
        .unwrap();
    assert_eq!(pruned.messages_pruned, 1);
    assert_eq!(store.erase(&[messages[2].id]).unwrap(), 1);
}

#[test]
fn nodes_share_queues_and_adopt_each_other() {
    fresh("oxim_t_nodes");
    let (Some(mut a), Some(mut b)) = (
        store("oxim_t_nodes", "node-a"),
        store("oxim_t_nodes", "node-b"),
    ) else {
        return;
    };
    let channel = ChannelId::new("lab").unwrap();
    let messages: Vec<Envelope> = (1..=4).map(envelope).collect();
    a.receive(&messages).unwrap();
    for message in &messages[..3] {
        a.finish_processing(message.id, &transformed(&message.raw), at(10))
            .unwrap();
    }
    // Best-effort queues are shared: each node claims a different message.
    let from_a = a
        .next_delivery(&channel, &lis(), QueueOrdering::BestEffort, at(11))
        .unwrap()
        .unwrap();
    let from_b = b
        .next_delivery(&channel, &lis(), QueueOrdering::BestEffort, at(11))
        .unwrap()
        .unwrap();
    assert_ne!(from_a.message_id, from_b.message_id);

    // B restarting releases only its own claim.
    let report = b.recover(at(12)).unwrap();
    assert_eq!(report.requeued, 1);
    assert!(report.unprocessed.is_empty());
    assert_eq!(a.queue_stats(&channel, &lis()).unwrap().sending, 1);

    // A dies: B adopts its claim and its unprocessed message.
    let adopted = b.adopt("node-a", at(13)).unwrap();
    assert_eq!(adopted.requeued, 1);
    assert_eq!(adopted.unprocessed, [messages[3].id]);
    assert_eq!(b.queue_stats(&channel, &lis()).unwrap().sending, 0);
}
