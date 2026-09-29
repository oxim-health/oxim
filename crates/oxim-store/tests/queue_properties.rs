//! Randomized delivery runs: no message is lost, and strict queues deliver
//! in arrival order whatever the sequence of failures, retries and crashes.

#![allow(clippy::unwrap_used)]

use oxim_model::{ChannelId, ConnectorId, DataType, Envelope, MessageId, MessageStatus, Timestamp};
use oxim_store::{
    Content, DeliveryOutcome, MessageStore, Processed, QueueOrdering, SqliteStore, Stage,
};
use proptest::prelude::*;

#[derive(Debug, Clone, Copy)]
enum Step {
    Deliver,
    FailOnce,
    Crash,
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![6 => Just(Step::Deliver), 3 => Just(Step::FailOnce), 1 => Just(Step::Crash)]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn strict_queues_deliver_everything_in_order(
        count in 1u64..12,
        steps in prop::collection::vec(step(), 0..60),
    ) {
        let channel = ChannelId::new("lab").unwrap();
        let lis = ConnectorId::new("lis").unwrap();
        let mut store = SqliteStore::open_in_memory().unwrap();
        let ids: Vec<MessageId> = (1..=count).map(|n| MessageId::from_parts(1000 + n, 0)).collect();
        let envelopes: Vec<Envelope> = ids
            .iter()
            .map(|id| {
                Envelope::new(*id, channel.clone(), ConnectorId::new("src").unwrap(),
                    Timestamp::from_unix_nanos(0), DataType::Raw, id.to_string().into_bytes())
            })
            .collect();
        store.receive(&envelopes).unwrap();
        for id in &ids {
            let processed = Processed {
                status: MessageStatus::Transformed,
                error: None,
                contents: vec![Content::for_destination(Stage::Encoded, lis.clone(), None, id.to_string().into_bytes())],
                queue: vec![lis.clone()],
                filtered: vec![],
            };
            store.finish_processing(*id, &processed, Timestamp::from_unix_nanos(0)).unwrap();
        }

        let mut clock = 1i64;
        let mut delivered: Vec<MessageId> = Vec::new();
        // The scripted steps, then plain delivery until the queue drains.
        let script = steps.into_iter().chain(std::iter::repeat(Step::Deliver));
        for step in script.take(10_000) {
            clock += 1_000_000_000;
            let now = Timestamp::from_unix_nanos(clock);
            if let Step::Crash = step {
                // A crash between claim and completion: claim, then recover.
                if store.next_delivery(&channel, &lis, QueueOrdering::Strict, now).unwrap().is_some() {
                    store.recover(now).unwrap();
                }
                continue;
            }
            let Some(delivery) = store.next_delivery(&channel, &lis, QueueOrdering::Strict, now).unwrap() else {
                if delivered.len() == ids.len() {
                    break;
                }
                continue;
            };
            prop_assert_eq!(delivery.payload.clone(), delivery.message_id.to_string().into_bytes());
            let outcome = match step {
                Step::FailOnce => DeliveryOutcome::Retry {
                    retry_at: Timestamp::from_unix_nanos(clock + 500_000_000),
                    error: "timeout".into(),
                },
                _ => DeliveryOutcome::Sent { response: None },
            };
            if matches!(outcome, DeliveryOutcome::Sent { .. }) {
                delivered.push(delivery.message_id);
            }
            store.complete_delivery(delivery.message_id, &lis, &outcome, now).unwrap();
        }

        prop_assert_eq!(&delivered, &ids);
        for id in &ids {
            prop_assert_eq!(store.message(*id).unwrap().unwrap().status, MessageStatus::Completed);
        }
        let stats = store.queue_stats(&channel, &lis).unwrap();
        prop_assert_eq!((stats.queued, stats.sending, stats.retrying), (0, 0, 0));
    }
}
