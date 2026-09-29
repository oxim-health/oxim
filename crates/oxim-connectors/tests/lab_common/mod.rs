//! Shared helpers for connector integration tests.

#![allow(
    dead_code,
    unreachable_pub,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use oxim_core::{
    ChannelConfig, ConnectorError, Engine, EngineOptions, Registry, SourceConnector, SourceContext,
    SubmitInfo, SystemClock, async_trait,
};
use oxim_model::{ChannelId, ConnectorId, Envelope, MessageId, MessageStatus, Timestamp};
use oxim_store::{
    AuditEvent, Content, Delivery, DeliveryOutcome, MessageQuery, MessageRecord, MessageStore,
    Processed, PrunePolicy, PruneReport, QueueOrdering, QueueStats, RecoveryReport, SqliteStore,
    Stage, StoreError, StoreResult,
};
use tokio::sync::{mpsc, oneshot};

/// Records when messages were stored, and can be told to fail or delay.
#[derive(Debug, Default)]
pub struct Probe {
    pub stored_at: Mutex<Vec<Instant>>,
    pub fail_receives: AtomicU32,
    pub delay_ms: AtomicU32,
}

/// A store that delegates to SQLite but reports to a [`Probe`].
pub struct ProbedStore {
    inner: SqliteStore,
    probe: Arc<Probe>,
}

impl ProbedStore {
    pub fn new(probe: Arc<Probe>) -> Self {
        Self {
            inner: SqliteStore::open_in_memory().unwrap(),
            probe,
        }
    }
}

impl MessageStore for ProbedStore {
    fn receive(&mut self, envelopes: &[Envelope]) -> StoreResult<()> {
        let delay = self.probe.delay_ms.load(Ordering::SeqCst);
        if delay > 0 {
            std::thread::sleep(Duration::from_millis(u64::from(delay)));
        }
        if self
            .probe
            .fail_receives
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            return Err(StoreError::InvalidState("disk full (simulated)".into()));
        }
        self.inner.receive(envelopes)?;
        self.probe.stored_at.lock().unwrap().push(Instant::now());
        Ok(())
    }
    fn finish_processing(
        &mut self,
        id: MessageId,
        processed: &Processed,
        now: Timestamp,
    ) -> StoreResult<()> {
        self.inner.finish_processing(id, processed, now)
    }
    fn reprocess(&mut self, id: MessageId) -> StoreResult<()> {
        self.inner.reprocess(id)
    }
    fn next_delivery(
        &mut self,
        channel: &ChannelId,
        destination: &ConnectorId,
        ordering: QueueOrdering,
        now: Timestamp,
    ) -> StoreResult<Option<Delivery>> {
        self.inner
            .next_delivery(channel, destination, ordering, now)
    }
    fn complete_delivery(
        &mut self,
        id: MessageId,
        destination: &ConnectorId,
        outcome: &DeliveryOutcome,
        now: Timestamp,
    ) -> StoreResult<()> {
        self.inner.complete_delivery(id, destination, outcome, now)
    }
    fn requeue(
        &mut self,
        id: MessageId,
        destination: &ConnectorId,
        now: Timestamp,
    ) -> StoreResult<()> {
        self.inner.requeue(id, destination, now)
    }
    fn recover(&mut self, now: Timestamp) -> StoreResult<RecoveryReport> {
        self.inner.recover(now)
    }
    fn release_in_flight(&mut self, channel: &ChannelId, now: Timestamp) -> StoreResult<u64> {
        self.inner.release_in_flight(channel, now)
    }
    fn message(&self, id: MessageId) -> StoreResult<Option<MessageRecord>> {
        self.inner.message(id)
    }
    fn content(
        &self,
        id: MessageId,
        stage: Stage,
        destination: Option<&ConnectorId>,
    ) -> StoreResult<Option<Content>> {
        self.inner.content(id, stage, destination)
    }
    fn list_messages(&self, query: &MessageQuery) -> StoreResult<Vec<MessageRecord>> {
        self.inner.list_messages(query)
    }
    fn queue_stats(
        &self,
        channel: &ChannelId,
        destination: &ConnectorId,
    ) -> StoreResult<QueueStats> {
        self.inner.queue_stats(channel, destination)
    }
    fn next_retry_at(
        &self,
        channel: &ChannelId,
        destination: &ConnectorId,
    ) -> StoreResult<Option<Timestamp>> {
        self.inner.next_retry_at(channel, destination)
    }
    fn prune(&mut self, policy: &PrunePolicy) -> StoreResult<PruneReport> {
        self.inner.prune(policy)
    }
    fn erase(&mut self, ids: &[MessageId]) -> StoreResult<u64> {
        self.inner.erase(ids)
    }
    fn record_audit(&mut self, event: &AuditEvent) -> StoreResult<()> {
        self.inner.record_audit(event)
    }
    fn audit_trail(
        &self,
        message: Option<MessageId>,
        limit: usize,
    ) -> StoreResult<Vec<AuditEvent>> {
        self.inner.audit_trail(message, limit)
    }
}

type Submission = (Vec<u8>, oneshot::Sender<Result<MessageId, String>>);

/// A source fed by the test.
#[derive(Debug)]
pub struct TestSource {
    inbox: tokio::sync::Mutex<mpsc::Receiver<Submission>>,
}

#[async_trait]
impl SourceConnector for TestSource {
    async fn run(&self, context: SourceContext) -> Result<(), ConnectorError> {
        let mut inbox = self.inbox.lock().await;
        loop {
            tokio::select! {
                () = context.cancelled() => return Ok(()),
                next = inbox.recv() => {
                    let Some((raw, reply)) = next else { return Ok(()) };
                    let result = context.submit(raw, SubmitInfo::default()).await;
                    let _ = reply.send(result.map_err(|e| e.to_string()));
                }
            }
        }
    }
}

pub struct Harness {
    pub engine: Engine,
    pub probe: Arc<Probe>,
    pub inbox: mpsc::Sender<Submission>,
}

pub async fn harness() -> Harness {
    let probe = Arc::new(Probe::default());
    let (inbox, rx) = mpsc::channel(16);
    let source = Arc::new(TestSource {
        inbox: tokio::sync::Mutex::new(rx),
    });
    let mut registry = Registry::new();
    oxim_connectors::register(&mut registry);
    registry.add_source("test", move |_| {
        Ok(source.clone() as Arc<dyn SourceConnector>)
    });
    let mut options = EngineOptions::default();
    options.idle_poll = Duration::from_millis(50);
    options.shutdown_grace = Duration::from_secs(2);
    options.source_restart_delay = Duration::from_millis(50);
    let engine = Engine::start(
        Box::new(ProbedStore::new(probe.clone())),
        registry,
        Arc::new(SystemClock),
        options,
    )
    .await
    .unwrap();
    Harness {
        engine,
        probe,
        inbox,
    }
}

impl Harness {
    pub async fn deploy(&self, yaml: &str) {
        self.engine
            .deploy(ChannelConfig::from_yaml(yaml).unwrap())
            .await
            .unwrap();
    }

    pub async fn submit(&self, raw: &[u8]) -> MessageId {
        let (reply, response) = oneshot::channel();
        self.inbox.send((raw.to_vec(), reply)).await.unwrap();
        response.await.unwrap().unwrap()
    }

    pub async fn messages(&self, channel: &str) -> Vec<MessageRecord> {
        let query = MessageQuery {
            channel: Some(ChannelId::new(channel).unwrap()),
            ..MessageQuery::default()
        };
        let mut records = self
            .engine
            .store()
            .run(move |store| store.list_messages(&query))
            .await
            .unwrap();
        records.reverse();
        records
    }

    pub async fn raw(&self, id: MessageId) -> Vec<u8> {
        self.engine
            .store()
            .run(move |store| store.content(id, Stage::Raw, None))
            .await
            .unwrap()
            .unwrap()
            .data
    }

    /// Waits until `channel` holds `count` messages in `status`.
    pub async fn wait_for(
        &self,
        channel: &str,
        count: usize,
        status: MessageStatus,
    ) -> Vec<MessageRecord> {
        for _ in 0..500 {
            let records = self.messages(channel).await;
            if records.iter().filter(|r| r.status == status).count() >= count {
                return records;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!(
            "channel {channel} did not reach {count} messages in {status}: {:?}",
            self.messages(channel).await
        );
    }
}

/// A local port that was free a moment ago.
pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Retries `connect` until the connector listens.
pub async fn connect(port: u16) -> tokio::net::TcpStream {
    for _ in 0..200 {
        if let Ok(stream) = tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
            return stream;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("nothing listens on port {port}");
}
