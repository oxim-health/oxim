//! Maintenance mode: paused sources stop while delivery goes on.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use oxim_core::{
    ChannelConfig, DestinationConnector, Engine, EngineOptions, Registry, SendError,
    SourceConnector, SourceContext, SubmitInfo, SystemClock, async_trait,
};
use oxim_store::{Delivery, SqliteStore};

/// Submits one message each time it starts, then runs until stopped.
#[derive(Debug, Default)]
struct Counting {
    starts: AtomicUsize,
    running: AtomicBool,
}

#[async_trait]
impl SourceConnector for Counting {
    async fn run(&self, context: SourceContext) -> Result<(), oxim_core::ConnectorError> {
        let n = self.starts.fetch_add(1, Ordering::SeqCst);
        self.running.store(true, Ordering::SeqCst);
        context
            .submit(format!("message {n}").into_bytes(), SubmitInfo::default())
            .await
            .unwrap();
        context.cancelled().await;
        self.running.store(false, Ordering::SeqCst);
        Ok(())
    }
}

/// Fails until opened, then records deliveries.
#[derive(Debug, Default)]
struct Gate {
    open: AtomicBool,
    delivered: AtomicUsize,
}

#[async_trait]
impl DestinationConnector for Gate {
    async fn send(&self, _delivery: &Delivery) -> Result<Option<Vec<u8>>, SendError> {
        if self.open.load(Ordering::SeqCst) {
            self.delivered.fetch_add(1, Ordering::SeqCst);
            Ok(None)
        } else {
            Err(SendError::temporary("closed"))
        }
    }
}

async fn wait(what: &str, condition: impl Fn() -> bool) {
    for _ in 0..500 {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting until {what}");
}

#[tokio::test(flavor = "multi_thread")]
async fn paused_sources_stop_while_queues_drain() {
    let source = Arc::new(Counting::default());
    let gate = Arc::new(Gate::default());
    let mut registry = Registry::new();
    let (s, g) = (source.clone(), gate.clone());
    registry
        .add_source("counting", move |_| {
            Ok(s.clone() as Arc<dyn SourceConnector>)
        })
        .add_destination("gate", move |_| {
            Ok(g.clone() as Arc<dyn DestinationConnector>)
        });
    let mut options = EngineOptions::default();
    options.idle_poll = Duration::from_millis(20);
    options.shutdown_grace = Duration::from_secs(2);
    let engine = Engine::start(
        Box::new(SqliteStore::open_in_memory().unwrap()),
        registry,
        Arc::new(SystemClock),
        options,
    )
    .await
    .unwrap();
    engine
        .deploy(
            ChannelConfig::from_yaml(
                "id: lab
source: {type: counting, data_type: raw}
destinations:
  - id: out
    type: gate
    queue: {retry: {initial_delay: 20ms, max_delay: 20ms}}
",
            )
            .unwrap(),
        )
        .await
        .unwrap();
    wait("the source runs", || source.running.load(Ordering::SeqCst)).await;

    engine.pause_sources();
    assert!(engine.sources_paused());
    wait("the source stops", || {
        !source.running.load(Ordering::SeqCst)
    })
    .await;
    // The queued message is still delivered while paused.
    gate.open.store(true, Ordering::SeqCst);
    wait("the queue drains", || {
        gate.delivered.load(Ordering::SeqCst) == 1
    })
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(source.starts.load(Ordering::SeqCst), 1);
    assert!(!source.running.load(Ordering::SeqCst));

    engine.resume_sources();
    wait("the source restarts", || {
        source.starts.load(Ordering::SeqCst) == 2
    })
    .await;
    wait("the new message is delivered", || {
        gate.delivered.load(Ordering::SeqCst) == 2
    })
    .await;

    // A channel deployed while paused starts with its source stopped.
    engine.pause_sources();
    wait("the source stops again", || {
        !source.running.load(Ordering::SeqCst)
    })
    .await;
    engine
        .redeploy(
            ChannelConfig::from_yaml(
                "id: lab\nsource: {type: counting, data_type: raw}\ndestinations:\n  - {id: out, type: gate}\n",
            )
            .unwrap(),
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(source.starts.load(Ordering::SeqCst), 2);
    engine.resume_sources();
    wait("the redeployed source starts", || {
        source.starts.load(Ordering::SeqCst) == 3
    })
    .await;
    engine.shutdown().await;
}
