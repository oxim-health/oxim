//! The engine: deploys channels and runs their sources, processors and
//! destination workers.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use oxim_model::{
    ChannelId, ConnectorId, Envelope, MessageId, MessageIdGenerator, MessageStatus, Timestamp,
};
use oxim_store::{DeliveryOutcome, MessageQuery, MessageStore, Processed, Stage};
use tokio::sync::{Notify, mpsc, watch};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::clock::Clock;
use crate::config::{ChannelConfig, RetryPolicy};
use crate::connector::{
    ChannelShared, DeliveryReport, DeliveryWatches, DestinationConnector, Job, SourceConnector,
    SourceContext,
};
use crate::error::EngineError;
use crate::registry::Registry;
use crate::store_actor::StoreHandle;

/// Tuning of the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct EngineOptions {
    /// Messages waiting for processing per channel before sources are
    /// slowed down.
    pub processing_queue: usize,
    /// How long an undeploy waits for tasks to finish before aborting them.
    pub shutdown_grace: Duration,
    /// Delay before a failed source connector is started again.
    pub source_restart_delay: Duration,
    /// How often an idle destination worker checks its queue even without
    /// being notified.
    pub idle_poll: Duration,
}

impl Default for EngineOptions {
    fn default() -> Self {
        Self {
            processing_queue: 1024,
            shutdown_grace: Duration::from_secs(30),
            source_restart_delay: Duration::from_secs(5),
            idle_poll: Duration::from_secs(30),
        }
    }
}

struct Running {
    config: ChannelConfig,
    cancel: CancellationToken,
    tasks: JoinSet<()>,
    jobs: mpsc::Sender<Job>,
    notifiers: Arc<BTreeMap<ConnectorId, Arc<Notify>>>,
}

struct Inner {
    store: StoreHandle,
    clock: Arc<dyn Clock>,
    ids: Arc<Mutex<MessageIdGenerator>>,
    registry: Registry,
    options: EngineOptions,
    channels: tokio::sync::Mutex<BTreeMap<ChannelId, Running>>,
    shutdown: CancellationToken,
    /// Whether sources are paused (maintenance mode).
    paused: watch::Sender<bool>,
}

/// The integration engine.
///
/// Cloning an `Engine` yields another handle to the same engine.
#[derive(Clone)]
pub struct Engine {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("registry", &self.inner.registry)
            .finish_non_exhaustive()
    }
}

/// Adds a duration to a timestamp, saturating at the end of the range.
pub(crate) fn add(timestamp: Timestamp, duration: Duration) -> Timestamp {
    let nanos = i64::try_from(duration.as_nanos()).unwrap_or(i64::MAX);
    Timestamp::from_unix_nanos(timestamp.unix_nanos().saturating_add(nanos))
}

/// The time from `now` until `target`, or zero if it has passed.
pub(crate) fn until(target: Timestamp, now: Timestamp) -> Duration {
    let nanos = target.unix_nanos().saturating_sub(now.unix_nanos());
    Duration::from_nanos(u64::try_from(nanos).unwrap_or(0))
}

impl Engine {
    /// Starts the engine on `store`, recovering from any previous crash:
    /// in-flight deliveries return to their queues, and messages that were
    /// received but not processed are processed when their channel is
    /// deployed.
    pub async fn start(
        store: Box<dyn MessageStore>,
        registry: Registry,
        clock: Arc<dyn Clock>,
        options: EngineOptions,
    ) -> Result<Self, EngineError> {
        let (store, _thread) = StoreHandle::spawn(store)?;
        let now = clock.now();
        let report = store.run(move |store| store.recover(now)).await?;
        info!(
            requeued = report.requeued,
            unprocessed = report.unprocessed.len(),
            "message store recovered"
        );
        Ok(Self {
            inner: Arc::new(Inner {
                store,
                clock,
                ids: Arc::new(Mutex::new(MessageIdGenerator::new())),
                registry,
                options,
                channels: tokio::sync::Mutex::new(BTreeMap::new()),
                shutdown: CancellationToken::new(),
                paused: watch::Sender::new(false),
            }),
        })
    }

    /// Stops every source (maintenance mode): listeners close and pollers
    /// stop, so senders see refused connections and retry later, while
    /// processing and delivery of stored messages go on. Channels deployed
    /// while paused start with their source stopped. The pause is not
    /// persisted.
    pub fn pause_sources(&self) {
        self.inner.paused.send_replace(true);
    }

    /// Starts the sources stopped by [`Engine::pause_sources`] again.
    pub fn resume_sources(&self) {
        self.inner.paused.send_replace(false);
    }

    /// Whether sources are paused.
    pub fn sources_paused(&self) -> bool {
        *self.inner.paused.borrow()
    }

    /// The store handle, for queries and operator actions.
    pub fn store(&self) -> &StoreHandle {
        &self.inner.store
    }

    /// The registry of component types.
    pub fn registry(&self) -> &Registry {
        &self.inner.registry
    }

    /// The engine's clock.
    pub fn clock(&self) -> &Arc<dyn Clock> {
        &self.inner.clock
    }

    /// The identifiers of the deployed channels.
    pub async fn deployed(&self) -> Vec<ChannelId> {
        self.inner.channels.lock().await.keys().cloned().collect()
    }

    /// The configuration of a deployed channel.
    pub async fn channel_config(&self, id: &ChannelId) -> Option<ChannelConfig> {
        self.inner
            .channels
            .lock()
            .await
            .get(id)
            .map(|running| running.config.clone())
    }

    /// Starts a channel. Messages of this channel that were received but
    /// not processed, for example before a crash, are processed first.
    pub async fn deploy(&self, config: ChannelConfig) -> Result<(), EngineError> {
        let inner = &self.inner;
        if inner.shutdown.is_cancelled() {
            return Err(EngineError::ShuttingDown);
        }
        let mut channels = inner.channels.lock().await;
        if channels.contains_key(&config.id) {
            return Err(EngineError::AlreadyDeployed(config.id));
        }
        let pipeline = Arc::new(inner.registry.compile(&config)?);
        let source = inner.registry.source(&config.source)?;
        let destinations = config
            .destinations
            .iter()
            .map(|destination| {
                Ok((
                    destination.clone(),
                    inner.registry.destination(destination)?,
                ))
            })
            .collect::<Result<Vec<_>, EngineError>>()?;

        let channel = config.id.clone();
        let now = inner.clock.now();
        let released = {
            let channel = channel.clone();
            inner
                .store
                .run(move |store| store.release_in_flight(&channel, now))
                .await?
        };
        if released > 0 {
            info!(%channel, released, "released deliveries left in flight");
        }

        let cancel = inner.shutdown.child_token();
        let mut tasks = JoinSet::new();
        let (jobs, job_queue) = mpsc::channel(inner.options.processing_queue.max(1));
        let notifiers: Arc<BTreeMap<ConnectorId, Arc<Notify>>> = Arc::new(
            destinations
                .iter()
                .map(|(destination, _)| (destination.id.clone(), Arc::new(Notify::new())))
                .collect(),
        );

        let watches = Arc::new(DeliveryWatches::default());
        let shared = Arc::new(ChannelShared {
            channel: channel.clone(),
            connector: config.source.id.clone(),
            data_type: config.source.data_type,
            store: inner.store.clone(),
            clock: inner.clock.clone(),
            ids: inner.ids.clone(),
            jobs: jobs.clone(),
            pipeline,
            notifiers: notifiers.clone(),
            watches: watches.clone(),
            response: config.source.response.clone(),
        });

        tasks.spawn(process_jobs(shared.clone(), job_queue, cancel.clone()));
        for (destination, connector) in destinations {
            let notify = notifiers
                .get(&destination.id)
                .cloned()
                .unwrap_or_else(|| Arc::new(Notify::new()));
            tasks.spawn(deliver(Worker {
                channel: channel.clone(),
                destination: destination.id.clone(),
                ordering: destination.queue.ordering.into(),
                retry: destination.queue.retry,
                connector,
                store: inner.store.clone(),
                clock: inner.clock.clone(),
                notify,
                watches: watches.clone(),
                cancel: cancel.clone(),
                idle_poll: inner.options.idle_poll,
            }));
        }
        tasks.spawn(run_source(
            source,
            shared,
            cancel.clone(),
            inner.paused.subscribe(),
            inner.options.source_restart_delay,
        ));
        tasks.spawn(requeue_unprocessed(
            channel.clone(),
            inner.store.clone(),
            jobs.clone(),
            now,
            cancel.clone(),
        ));

        info!(%channel, "channel deployed");
        channels.insert(
            channel,
            Running {
                config,
                cancel,
                tasks,
                jobs,
                notifiers,
            },
        );
        Ok(())
    }

    /// Schedules a stored message of a deployed channel for processing, for
    /// example after [`MessageStore::reprocess`] reset it. Messages that are
    /// not waiting for processing are skipped by the processor.
    pub async fn process_stored(
        &self,
        channel: &ChannelId,
        id: MessageId,
    ) -> Result<(), EngineError> {
        let jobs = self
            .inner
            .channels
            .lock()
            .await
            .get(channel)
            .map(|running| running.jobs.clone())
            .ok_or_else(|| EngineError::NotDeployed(channel.clone()))?;
        jobs.send(Job::Stored(id))
            .await
            .map_err(|_| EngineError::ShuttingDown)
    }

    /// Wakes the delivery worker of a destination so it checks its queue
    /// now, for example after a delivery was requeued.
    pub async fn wake_destination(
        &self,
        channel: &ChannelId,
        destination: &ConnectorId,
    ) -> Result<(), EngineError> {
        let channels = self.inner.channels.lock().await;
        let running = channels
            .get(channel)
            .ok_or_else(|| EngineError::NotDeployed(channel.clone()))?;
        if let Some(notify) = running.notifiers.get(destination) {
            notify.notify_one();
        }
        Ok(())
    }

    /// Stops a channel. Messages already received stay stored and are
    /// processed and delivered when the channel is deployed again.
    pub async fn undeploy(&self, id: &ChannelId) -> Result<(), EngineError> {
        let running = self
            .inner
            .channels
            .lock()
            .await
            .remove(id)
            .ok_or_else(|| EngineError::NotDeployed(id.clone()))?;
        self.stop(id, running).await;
        Ok(())
    }

    /// Replaces a deployed channel with a new configuration, or deploys it
    /// if it is not running.
    pub async fn redeploy(&self, config: ChannelConfig) -> Result<(), EngineError> {
        match self.undeploy(&config.id).await {
            Ok(()) | Err(EngineError::NotDeployed(_)) => {}
            Err(other) => return Err(other),
        }
        self.deploy(config).await
    }

    /// Stops every channel. The engine accepts no new deployments afterwards.
    pub async fn shutdown(&self) {
        self.inner.shutdown.cancel();
        let channels = std::mem::take(&mut *self.inner.channels.lock().await);
        for (id, running) in channels {
            self.stop(&id, running).await;
        }
    }

    async fn stop(&self, id: &ChannelId, mut running: Running) {
        running.cancel.cancel();
        let grace = self.inner.options.shutdown_grace;
        let finished = tokio::time::timeout(grace, async {
            while running.tasks.join_next().await.is_some() {}
        })
        .await;
        if finished.is_err() {
            warn!(channel = %id, "channel tasks did not stop in time; aborting them");
            running.tasks.shutdown().await;
        }
        info!(channel = %id, "channel undeployed");
    }
}

/// Runs a channel's source until the channel stops, restarting it after
/// failures, and stopping it while sources are paused.
async fn run_source(
    source: Arc<dyn SourceConnector>,
    shared: Arc<ChannelShared>,
    cancel: CancellationToken,
    mut paused: watch::Receiver<bool>,
    restart_delay: Duration,
) {
    let channel = shared.channel.clone();
    let mut watching = true;
    loop {
        while watching && *paused.borrow_and_update() {
            tokio::select! {
                () = cancel.cancelled() => return,
                changed = paused.changed() => if changed.is_err() { watching = false },
            }
        }
        // Each run gets its own token, so a pause stops the source alone.
        let run_cancel = cancel.child_token();
        let context = SourceContext::new(shared.clone(), run_cancel.clone());
        let run = source.run(context);
        tokio::pin!(run);
        let mut pausing = false;
        let result = loop {
            tokio::select! {
                result = &mut run => break result,
                changed = paused.changed(), if watching && !pausing => match changed {
                    Err(_) => watching = false,
                    Ok(()) => if *paused.borrow_and_update() {
                        pausing = true;
                        run_cancel.cancel();
                    },
                },
            }
        };
        if cancel.is_cancelled() {
            return;
        }
        if pausing {
            info!(%channel, "source paused");
            continue;
        }
        match result {
            Ok(()) => warn!(%channel, "source connector stopped; restarting"),
            Err(e) => error!(%channel, error = %e, "source connector failed; restarting"),
        }
        tokio::select! {
            () = cancel.cancelled() => return,
            () = tokio::time::sleep(restart_delay) => {}
        }
    }
}

/// Queues messages of `channel` that were received before `deployed_at`
/// but never processed, oldest first.
async fn requeue_unprocessed(
    channel: ChannelId,
    store: StoreHandle,
    jobs: mpsc::Sender<Job>,
    deployed_at: Timestamp,
    cancel: CancellationToken,
) {
    let mut ids: Vec<MessageId> = Vec::new();
    let mut before = None;
    loop {
        let query = MessageQuery {
            channel: Some(channel.clone()),
            status: Some(MessageStatus::Received),
            until: Some(deployed_at),
            before,
            limit: 1000,
            ..MessageQuery::default()
        };
        let page = match store.run(move |store| store.list_messages(&query)).await {
            Ok(page) => page,
            Err(e) => {
                error!(%channel, error = %e, "cannot list unprocessed messages");
                return;
            }
        };
        let Some(last) = page.last() else { break };
        before = Some(last.id);
        ids.extend(page.iter().map(|record| record.id));
    }
    if !ids.is_empty() {
        info!(%channel, count = ids.len(), "processing messages left from an earlier run");
    }
    for id in ids.into_iter().rev() {
        tokio::select! {
            () = cancel.cancelled() => return,
            sent = jobs.send(Job::Stored(id)) => if sent.is_err() { return },
        }
    }
}

/// Loads a stored message that still needs processing.
fn load_unprocessed(
    store: &mut dyn MessageStore,
    id: MessageId,
) -> oxim_store::StoreResult<Option<Envelope>> {
    let Some(record) = store.message(id)? else {
        return Ok(None);
    };
    if record.status != MessageStatus::Received {
        return Ok(None);
    }
    let Some(raw) = store.content(id, Stage::Raw, None)? else {
        return Ok(None);
    };
    Ok(Some(Envelope {
        id,
        channel: record.channel,
        connector: record.connector,
        received_at: record.received_at,
        data_type: record.data_type,
        raw: raw.data,
        peer: record.peer,
        device: record.device,
        correlation_id: record.correlation_id,
        metadata: record.metadata,
    }))
}

/// Runs a message through the channel pipeline, records the result and
/// wakes the destination workers. Returns what was recorded.
pub(crate) async fn process_and_record(shared: &ChannelShared, envelope: Envelope) -> Processed {
    let channel = &shared.channel;
    let id = envelope.id;
    let steps = shared.pipeline.clone();
    let processed = match tokio::task::spawn_blocking(move || steps.process(envelope)).await {
        Ok(processed) => processed,
        Err(e) => Processed {
            status: MessageStatus::Error,
            error: Some(format!("processing panicked: {e}")),
            contents: Vec::new(),
            queue: Vec::new(),
            filtered: Vec::new(),
        },
    };
    if let Some(reason) = &processed.error {
        warn!(%channel, %id, error = %reason, "message processing failed");
    }
    let now = shared.clock.now();
    let record = processed.clone();
    match shared
        .store
        .run(move |store| store.finish_processing(id, &record, now))
        .await
    {
        Ok(()) => {
            debug!(%channel, %id, destinations = processed.queue.len(), "message processed");
            for destination in &processed.queue {
                if let Some(notify) = shared.notifiers.get(destination) {
                    notify.notify_one();
                }
            }
        }
        Err(e) => error!(%channel, %id, error = %e, "cannot record processing result"),
    }
    processed
}

async fn process_jobs(
    shared: Arc<ChannelShared>,
    mut jobs: mpsc::Receiver<Job>,
    cancel: CancellationToken,
) {
    loop {
        let job = tokio::select! {
            () = cancel.cancelled() => return,
            job = jobs.recv() => match job {
                Some(job) => job,
                None => return,
            },
        };
        let envelope = match job {
            Job::Fresh(envelope) => envelope,
            Job::Stored(id) => match shared
                .store
                .run(move |store| load_unprocessed(store, id))
                .await
            {
                Ok(Some(envelope)) => envelope,
                Ok(None) => continue,
                Err(e) => {
                    error!(channel = %shared.channel, %id, error = %e, "cannot load message for processing");
                    continue;
                }
            },
        };
        process_and_record(&shared, envelope).await;
    }
}

struct Worker {
    channel: ChannelId,
    destination: ConnectorId,
    ordering: oxim_store::QueueOrdering,
    retry: RetryPolicy,
    connector: Arc<dyn DestinationConnector>,
    store: StoreHandle,
    clock: Arc<dyn Clock>,
    notify: Arc<Notify>,
    watches: Arc<DeliveryWatches>,
    cancel: CancellationToken,
    idle_poll: Duration,
}

/// Delivers the queue of one destination until the channel stops.
async fn deliver(worker: Worker) {
    let Worker {
        channel,
        destination,
        ordering,
        retry,
        connector,
        store,
        clock,
        notify,
        watches,
        cancel,
        idle_poll,
    } = worker;
    loop {
        // Send everything that is due.
        while !cancel.is_cancelled() {
            let now = clock.now();
            let (queue_channel, queue_destination) = (channel.clone(), destination.clone());
            let next = store
                .run(move |store| {
                    store.next_delivery(&queue_channel, &queue_destination, ordering, now)
                })
                .await;
            let delivery = match next {
                Ok(Some(delivery)) => delivery,
                Ok(None) => break,
                Err(e) => {
                    error!(%channel, %destination, error = %e, "cannot read the delivery queue");
                    break;
                }
            };
            // A send in progress is finished even when the channel stops, so
            // no delivery is left half done.
            let result = connector.send(&delivery).await;
            let now = clock.now();
            let attempts = delivery.attempts + 1;
            let outcome = match result {
                Ok(response) => DeliveryOutcome::Sent { response },
                Err(e) if e.permanent => {
                    warn!(%channel, %destination, id = %delivery.message_id, error = %e, "delivery rejected");
                    DeliveryOutcome::Failed { error: e.message }
                }
                Err(e) => match retry.delay_after(attempts) {
                    Some(delay) => {
                        debug!(%channel, %destination, id = %delivery.message_id, attempts, error = %e, "delivery failed; will retry");
                        DeliveryOutcome::Retry {
                            retry_at: add(now, delay),
                            error: e.message,
                        }
                    }
                    None => {
                        warn!(%channel, %destination, id = %delivery.message_id, attempts, error = %e, "delivery failed; giving up");
                        DeliveryOutcome::Failed {
                            error: format!("{} (gave up after {attempts} attempts)", e.message),
                        }
                    }
                },
            };
            let report = match &outcome {
                DeliveryOutcome::Sent { response } => Some(DeliveryReport {
                    delivered: true,
                    response: response.clone(),
                    error: None,
                }),
                DeliveryOutcome::Failed { error } => Some(DeliveryReport {
                    delivered: false,
                    response: None,
                    error: Some(error.clone()),
                }),
                DeliveryOutcome::Retry { .. } => None,
            };
            let (id, target) = (delivery.message_id, destination.clone());
            if let Err(e) = store
                .run(move |store| store.complete_delivery(id, &target, &outcome, now))
                .await
            {
                error!(%channel, %destination, %id, error = %e, "cannot record delivery outcome");
                break;
            }
            if let Some(report) = report {
                watches.complete(id, &destination, report);
            }
        }

        // Sleep until new work arrives, the next retry is due or the channel
        // stops.
        let (retry_channel, retry_destination) = (channel.clone(), destination.clone());
        let wait = match store
            .run(move |store| store.next_retry_at(&retry_channel, &retry_destination))
            .await
        {
            Ok(Some(at)) => until(at, clock.now()).min(idle_poll),
            _ => idle_poll,
        };
        tokio::select! {
            () = cancel.cancelled() => return,
            () = notify.notified() => {}
            () = tokio::time::sleep(wait) => {}
        }
    }
}
