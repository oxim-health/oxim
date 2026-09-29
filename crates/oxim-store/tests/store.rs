//! Behavior of the SQLite message store.

#![allow(clippy::unwrap_used)]

use oxim_model::{
    ChannelId, ConnectorId, DataType, DestinationStatus, Envelope, MessageId, MessageStatus,
    Timestamp,
};
use oxim_store::{
    AuditEvent, Content, DeliveryOutcome, MessageQuery, MessageStore, Processed, PrunePolicy,
    QueueOrdering, SqliteStore, Stage, StoreError,
};

const SECOND: i64 = 1_000_000_000;

fn at(seconds: i64) -> Timestamp {
    Timestamp::from_unix_nanos(1_790_000_000 * SECOND + seconds * SECOND)
}

fn channel() -> ChannelId {
    ChannelId::new("lab").unwrap()
}

fn dest(name: &str) -> ConnectorId {
    ConnectorId::new(name).unwrap()
}

fn envelope(n: u64) -> Envelope {
    let mut envelope = Envelope::new(
        MessageId::from_parts(1_790_000_000_000 + n, u128::from(n)),
        channel(),
        dest("analyzer"),
        at(n as i64),
        DataType::Astm,
        format!("H|\\^&\rR|1|^^^GLU|{n}\rL|1\r").into_bytes(),
    );
    envelope.peer = Some("192.168.1.37:5100".into());
    envelope
        .metadata
        .insert("device".into(), "chemistry-1".into());
    envelope
}

fn transformed_to(destinations: &[&str]) -> Processed {
    Processed {
        status: MessageStatus::Transformed,
        error: None,
        contents: destinations
            .iter()
            .map(|d| {
                Content::for_destination(
                    Stage::Encoded,
                    dest(d),
                    Some(DataType::Hl7V2),
                    format!("MSH|^~\\&|OXIM|{d}\r").into_bytes(),
                )
            })
            .collect(),
        queue: destinations.iter().map(|d| dest(d)).collect(),
        filtered: Vec::new(),
    }
}

fn store_with(count: u64, destinations: &[&str]) -> (SqliteStore, Vec<MessageId>) {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let envelopes: Vec<_> = (1..=count).map(envelope).collect();
    store.receive(&envelopes).unwrap();
    for envelope in &envelopes {
        store
            .finish_processing(envelope.id, &transformed_to(destinations), at(100))
            .unwrap();
    }
    (store, envelopes.iter().map(|e| e.id).collect())
}

#[test]
fn stores_messages_durably_across_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("oxim.db");
    let original = envelope(1);
    {
        let mut store = SqliteStore::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), 1);
        store.receive(std::slice::from_ref(&original)).unwrap();
    }
    let store = SqliteStore::open(&path).unwrap();
    let record = store.message(original.id).unwrap().unwrap();
    assert_eq!(record.status, MessageStatus::Received);
    assert_eq!(record.channel, channel());
    assert_eq!(record.received_at, original.received_at);
    assert_eq!(record.peer.as_deref(), Some("192.168.1.37:5100"));
    assert_eq!(record.metadata, original.metadata);
    let raw = store
        .content(original.id, Stage::Raw, None)
        .unwrap()
        .unwrap();
    assert_eq!(raw.data, original.raw);
    assert_eq!(raw.data_type, Some(DataType::Astm));
}

#[test]
fn refuses_newer_schemas() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("oxim.db");
    drop(SqliteStore::open(&path).unwrap());
    // Bump the schema version as a future OXIM release would.
    rusqlite::Connection::open(&path)
        .unwrap()
        .pragma_update(None, "user_version", 99)
        .unwrap();
    assert!(matches!(
        SqliteStore::open(&path),
        Err(StoreError::SchemaTooNew { found: 99, .. })
    ));
}

#[test]
fn completes_messages_without_queued_destinations() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let message = envelope(1);
    store.receive(std::slice::from_ref(&message)).unwrap();
    let processed = Processed {
        filtered: vec![dest("archive")],
        ..transformed_to(&[])
    };
    store
        .finish_processing(message.id, &processed, at(1))
        .unwrap();
    let record = store.message(message.id).unwrap().unwrap();
    assert_eq!(record.status, MessageStatus::Completed);
    assert_eq!(record.destinations[0].status, DestinationStatus::Filtered);
}

#[test]
fn delivers_in_strict_order_and_retries_the_head() {
    let (mut store, ids) = store_with(2, &["lis"]);
    let lis = dest("lis");

    let first = store
        .next_delivery(&channel(), &lis, QueueOrdering::Strict, at(100))
        .unwrap()
        .unwrap();
    assert_eq!(first.message_id, ids[0]);
    assert_eq!(first.attempts, 0);
    assert!(first.payload.starts_with(b"MSH|"));
    // The head is in flight: strict ordering holds back the second message.
    assert!(
        store
            .next_delivery(&channel(), &lis, QueueOrdering::Strict, at(100))
            .unwrap()
            .is_none()
    );

    store
        .complete_delivery(
            ids[0],
            &lis,
            &DeliveryOutcome::Retry {
                retry_at: at(130),
                error: "connection refused".into(),
            },
            at(101),
        )
        .unwrap();
    assert_eq!(
        store.next_retry_at(&channel(), &lis).unwrap(),
        Some(at(130))
    );
    assert!(
        store
            .next_delivery(&channel(), &lis, QueueOrdering::Strict, at(120))
            .unwrap()
            .is_none()
    );
    // Best-effort ordering skips the waiting head.
    let other = store
        .next_delivery(&channel(), &lis, QueueOrdering::BestEffort, at(120))
        .unwrap()
        .unwrap();
    assert_eq!(other.message_id, ids[1]);
    store
        .complete_delivery(
            ids[1],
            &lis,
            &DeliveryOutcome::Sent { response: None },
            at(121),
        )
        .unwrap();

    let retried = store
        .next_delivery(&channel(), &lis, QueueOrdering::Strict, at(130))
        .unwrap()
        .unwrap();
    assert_eq!((retried.message_id, retried.attempts), (ids[0], 1));
    store
        .complete_delivery(
            ids[0],
            &lis,
            &DeliveryOutcome::Sent {
                response: Some(b"MSH|^~\\&|LIS\rMSA|AA|1\r".to_vec()),
            },
            at(131),
        )
        .unwrap();

    let record = store.message(ids[0]).unwrap().unwrap();
    assert_eq!(record.status, MessageStatus::Completed);
    assert_eq!(record.destinations[0].status, DestinationStatus::Sent);
    assert_eq!(record.destinations[0].attempts, 2);
    let response = store
        .content(ids[0], Stage::Response, Some(&lis))
        .unwrap()
        .unwrap();
    assert!(response.data.ends_with(b"MSA|AA|1\r"));
}

#[test]
fn destinations_are_independent() {
    let (mut store, ids) = store_with(1, &["lis", "archive"]);
    let (lis, archive) = (dest("lis"), dest("archive"));
    store
        .next_delivery(&channel(), &lis, QueueOrdering::Strict, at(100))
        .unwrap()
        .unwrap();
    store
        .complete_delivery(
            ids[0],
            &lis,
            &DeliveryOutcome::Failed {
                error: "rejected".into(),
            },
            at(101),
        )
        .unwrap();
    // The archive queue is unaffected by the LIS failure.
    let delivery = store
        .next_delivery(&channel(), &archive, QueueOrdering::Strict, at(101))
        .unwrap()
        .unwrap();
    assert!(delivery.payload.ends_with(b"archive\r"));
    store
        .complete_delivery(
            ids[0],
            &archive,
            &DeliveryOutcome::Sent { response: None },
            at(102),
        )
        .unwrap();

    let record = store.message(ids[0]).unwrap().unwrap();
    assert_eq!(record.status, MessageStatus::Completed);
    let stats = store.queue_stats(&channel(), &lis).unwrap();
    assert_eq!(stats.failed, 1);

    store.requeue(ids[0], &lis, at(200)).unwrap();
    let record = store.message(ids[0]).unwrap().unwrap();
    assert_eq!(record.status, MessageStatus::Transformed);
    let again = store
        .next_delivery(&channel(), &lis, QueueOrdering::Strict, at(200))
        .unwrap()
        .unwrap();
    assert_eq!(again.attempts, 1);
}

#[test]
fn recovers_after_a_crash() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("oxim.db");
    let (sent, unprocessed) = (envelope(1), envelope(2));
    {
        let mut store = SqliteStore::open(&path).unwrap();
        store.receive(&[sent.clone(), unprocessed.clone()]).unwrap();
        store
            .finish_processing(sent.id, &transformed_to(&["lis"]), at(10))
            .unwrap();
        store
            .next_delivery(&channel(), &dest("lis"), QueueOrdering::Strict, at(11))
            .unwrap()
            .unwrap();
        // Crash: the delivery is in flight and message 2 was never processed.
    }
    let mut store = SqliteStore::open(&path).unwrap();
    let report = store.recover(at(20)).unwrap();
    assert_eq!(report.requeued, 1);
    assert_eq!(report.unprocessed, vec![unprocessed.id]);
    let delivery = store
        .next_delivery(&channel(), &dest("lis"), QueueOrdering::Strict, at(21))
        .unwrap()
        .unwrap();
    assert_eq!(delivery.message_id, sent.id);
}

#[test]
fn releases_in_flight_deliveries_of_one_channel() {
    let (mut store, ids) = store_with(1, &["lis"]);
    store
        .next_delivery(&channel(), &dest("lis"), QueueOrdering::Strict, at(100))
        .unwrap()
        .unwrap();
    let other = ChannelId::new("other").unwrap();
    assert_eq!(store.release_in_flight(&other, at(101)).unwrap(), 0);
    assert_eq!(store.release_in_flight(&channel(), at(101)).unwrap(), 1);
    let again = store
        .next_delivery(&channel(), &dest("lis"), QueueOrdering::Strict, at(102))
        .unwrap()
        .unwrap();
    assert_eq!(again.message_id, ids[0]);
}

#[test]
fn guards_invalid_transitions() {
    let (mut store, ids) = store_with(1, &["lis"]);
    let lis = dest("lis");
    assert!(matches!(
        store.finish_processing(ids[0], &transformed_to(&["lis"]), at(1)),
        Err(StoreError::InvalidState(_))
    ));
    assert!(matches!(
        store.complete_delivery(
            ids[0],
            &lis,
            &DeliveryOutcome::Sent { response: None },
            at(1)
        ),
        Err(StoreError::InvalidState(_))
    ));
    assert!(matches!(
        store.complete_delivery(
            ids[0],
            &dest("nowhere"),
            &DeliveryOutcome::Sent { response: None },
            at(1)
        ),
        Err(StoreError::DeliveryNotFound(..))
    ));
    assert!(matches!(
        store.requeue(ids[0], &lis, at(1)),
        Err(StoreError::InvalidState(_))
    ));

    let mut other = SqliteStore::open_in_memory().unwrap();
    let message = envelope(7);
    other.receive(std::slice::from_ref(&message)).unwrap();
    let mut missing_encoding = transformed_to(&["lis"]);
    missing_encoding.contents.clear();
    assert!(matches!(
        other.finish_processing(message.id, &missing_encoding, at(1)),
        Err(StoreError::InvalidState(_))
    ));
    let mut replaces_raw = transformed_to(&[]);
    replaces_raw
        .contents
        .push(Content::new(Stage::Raw, None, b"x".to_vec()));
    assert!(matches!(
        other.finish_processing(message.id, &replaces_raw, at(1)),
        Err(StoreError::InvalidState(_))
    ));
    assert!(matches!(
        other.finish_processing(MessageId::from_parts(1, 1), &transformed_to(&[]), at(1)),
        Err(StoreError::MessageNotFound(_))
    ));
}

#[test]
fn reprocesses_from_the_raw_content() {
    let (mut store, ids) = store_with(1, &["lis"]);
    store.reprocess(ids[0]).unwrap();
    let record = store.message(ids[0]).unwrap().unwrap();
    assert_eq!(record.status, MessageStatus::Received);
    assert!(record.destinations.is_empty());
    assert!(store.content(ids[0], Stage::Raw, None).unwrap().is_some());
    assert!(
        store
            .content(ids[0], Stage::Encoded, Some(&dest("lis")))
            .unwrap()
            .is_none()
    );
    store
        .finish_processing(ids[0], &transformed_to(&["lis"]), at(300))
        .unwrap();
}

#[test]
fn lists_messages_newest_first_with_filters() {
    let (mut store, ids) = store_with(5, &["lis"]);
    let lis = dest("lis");
    store
        .next_delivery(&channel(), &lis, QueueOrdering::Strict, at(100))
        .unwrap();
    store
        .complete_delivery(
            ids[0],
            &lis,
            &DeliveryOutcome::Failed { error: "x".into() },
            at(100),
        )
        .unwrap();

    let page = store
        .list_messages(&MessageQuery {
            limit: 2,
            ..MessageQuery::default()
        })
        .unwrap();
    assert_eq!(
        page.iter().map(|m| m.id).collect::<Vec<_>>(),
        [ids[4], ids[3]]
    );
    let next = store
        .list_messages(&MessageQuery {
            limit: 2,
            before: Some(ids[3]),
            ..MessageQuery::default()
        })
        .unwrap();
    assert_eq!(
        next.iter().map(|m| m.id).collect::<Vec<_>>(),
        [ids[2], ids[1]]
    );

    let failed = store
        .list_messages(&MessageQuery {
            destination_status: Some(DestinationStatus::Failed),
            ..MessageQuery::default()
        })
        .unwrap();
    assert_eq!(failed.iter().map(|m| m.id).collect::<Vec<_>>(), [ids[0]]);

    let window = store
        .list_messages(&MessageQuery {
            from: Some(at(2)),
            until: Some(at(4)),
            channel: Some(channel()),
            status: Some(MessageStatus::Transformed),
            ..MessageQuery::default()
        })
        .unwrap();
    assert_eq!(
        window.iter().map(|m| m.id).collect::<Vec<_>>(),
        [ids[2], ids[1]]
    );

    let stats = store.queue_stats(&channel(), &lis).unwrap();
    assert_eq!((stats.queued, stats.failed), (4, 1));
    assert_eq!(stats.oldest_pending_at, Some(at(2)));
}

#[test]
fn prunes_only_final_messages() {
    let (mut store, ids) = store_with(3, &["lis"]);
    let lis = dest("lis");
    for id in &ids[..2] {
        store
            .next_delivery(&channel(), &lis, QueueOrdering::Strict, at(100))
            .unwrap()
            .unwrap();
        store
            .complete_delivery(
                *id,
                &lis,
                &DeliveryOutcome::Sent { response: None },
                at(100),
            )
            .unwrap();
    }
    let report = store
        .prune(&PrunePolicy {
            contents_before: Some(at(1000)),
            messages_before: None,
        })
        .unwrap();
    assert_eq!(report.contents_pruned, 2);
    assert!(store.content(ids[0], Stage::Raw, None).unwrap().is_none());
    assert!(store.content(ids[2], Stage::Raw, None).unwrap().is_some());

    let report = store
        .prune(&PrunePolicy {
            contents_before: None,
            messages_before: Some(at(2)),
        })
        .unwrap();
    assert_eq!(report.messages_pruned, 1);
    assert!(store.message(ids[0]).unwrap().is_none());
    assert!(store.message(ids[1]).unwrap().is_some());

    assert_eq!(store.erase(&[ids[1], ids[2], ids[0]]).unwrap(), 2);
    assert!(store.message(ids[2]).unwrap().is_none());
}

#[test]
fn keeps_an_audit_trail() {
    let (mut store, ids) = store_with(1, &["lis"]);
    for action in ["message.viewed", "message.requeued"] {
        store
            .record_audit(&AuditEvent {
                at: at(500),
                action: action.into(),
                actor: "operator".into(),
                message_id: Some(ids[0]),
                channel: Some(channel()),
                detail: None,
            })
            .unwrap();
    }
    store
        .record_audit(&AuditEvent {
            at: at(501),
            action: "channel.deployed".into(),
            actor: "admin".into(),
            message_id: None,
            channel: Some(channel()),
            detail: Some("revision 3".into()),
        })
        .unwrap();
    let trail = store.audit_trail(Some(ids[0]), 10).unwrap();
    assert_eq!(
        trail.iter().map(|e| e.action.as_str()).collect::<Vec<_>>(),
        ["message.requeued", "message.viewed"]
    );
    // The audit trail outlives erased messages.
    store.erase(&ids).unwrap();
    assert_eq!(store.audit_trail(None, 10).unwrap().len(), 3);
}

#[test]
fn counts_messages_and_deliveries_by_status() {
    let (mut store, ids) = store_with(2, &["lis"]);
    store
        .next_delivery(&channel(), &dest("lis"), QueueOrdering::Strict, at(100))
        .unwrap()
        .unwrap();
    store
        .complete_delivery(
            ids[0],
            &dest("lis"),
            &DeliveryOutcome::Sent { response: None },
            at(100),
        )
        .unwrap();
    let counts = store.status_counts().unwrap();
    assert!(
        counts
            .messages
            .contains(&(channel(), MessageStatus::Completed, 1))
    );
    assert!(
        counts
            .messages
            .contains(&(channel(), MessageStatus::Transformed, 1))
    );
    assert!(
        counts
            .deliveries
            .contains(&(channel(), dest("lis"), DestinationStatus::Sent, 1))
    );
    assert!(
        counts
            .deliveries
            .contains(&(channel(), dest("lis"), DestinationStatus::Queued, 1))
    );
}
