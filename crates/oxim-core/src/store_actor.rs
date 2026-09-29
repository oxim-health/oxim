//! The thread that owns the message store.
//!
//! Store operations are blocking, so one dedicated thread owns the store
//! and the async engine talks to it through a channel. Newly received
//! messages that arrive while a commit is running are written together in
//! the next transaction (group commit), so durability costs one sync per
//! batch instead of one per message.

use std::sync::mpsc;
use std::thread::JoinHandle;

use oxim_model::Envelope;
use oxim_store::{MessageStore, StoreResult};
use tokio::sync::oneshot;

use crate::error::EngineError;

/// The largest number of received messages committed in one transaction.
const MAX_BATCH: usize = 512;

type Job = Box<dyn FnOnce(&mut dyn MessageStore) + Send>;

enum Command {
    Receive(Envelope, oneshot::Sender<StoreResult<()>>),
    Run(Job),
}

/// A cloneable handle to the store thread.
#[derive(Debug, Clone)]
pub struct StoreHandle {
    tx: mpsc::Sender<Command>,
}

impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Receive(envelope, _) => write!(f, "Receive({})", envelope.id),
            Self::Run(_) => f.write_str("Run"),
        }
    }
}

impl StoreHandle {
    /// Starts the store thread. It stops when every handle is dropped.
    pub fn spawn(store: Box<dyn MessageStore>) -> Result<(Self, JoinHandle<()>), EngineError> {
        let (tx, rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("oxim-store".into())
            .spawn(move || serve(store, &rx))
            .map_err(|e| EngineError::Config(format!("cannot start the store thread: {e}")))?;
        Ok((Self { tx }, thread))
    }

    /// Stores a newly received message durably. When this returns `Ok`, the
    /// message survives a crash and the sender may be acknowledged.
    pub async fn receive(&self, envelope: Envelope) -> Result<(), EngineError> {
        let (reply, response) = oneshot::channel();
        self.tx
            .send(Command::Receive(envelope, reply))
            .map_err(|_| EngineError::ShuttingDown)?;
        response
            .await
            .map_err(|_| EngineError::ShuttingDown)?
            .map_err(EngineError::Store)
    }

    /// Runs `operation` on the store thread and returns its result.
    pub async fn run<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut dyn MessageStore) -> StoreResult<T> + Send + 'static,
    ) -> Result<T, EngineError> {
        let (reply, response) = oneshot::channel();
        let job: Job = Box::new(move |store| {
            let _ = reply.send(operation(store));
        });
        self.tx
            .send(Command::Run(job))
            .map_err(|_| EngineError::ShuttingDown)?;
        response
            .await
            .map_err(|_| EngineError::ShuttingDown)?
            .map_err(EngineError::Store)
    }
}

fn serve(mut store: Box<dyn MessageStore>, rx: &mpsc::Receiver<Command>) {
    let mut pending: Option<Command> = None;
    loop {
        let command = match pending.take() {
            Some(command) => command,
            None => match rx.recv() {
                Ok(command) => command,
                Err(_) => return,
            },
        };
        match command {
            Command::Run(job) => job(store.as_mut()),
            Command::Receive(first, reply) => {
                let mut envelopes = vec![first];
                let mut replies = vec![reply];
                while envelopes.len() < MAX_BATCH {
                    match rx.try_recv() {
                        Ok(Command::Receive(envelope, reply)) => {
                            envelopes.push(envelope);
                            replies.push(reply);
                        }
                        Ok(other) => {
                            pending = Some(other);
                            break;
                        }
                        Err(_) => break,
                    }
                }
                match store.receive(&envelopes) {
                    Ok(()) => {
                        for reply in replies {
                            let _ = reply.send(Ok(()));
                        }
                    }
                    Err(error) if envelopes.len() == 1 => {
                        if let Some(reply) = replies.into_iter().next() {
                            let _ = reply.send(Err(error));
                        }
                    }
                    Err(_) => {
                        // The batch was rolled back; store the messages one by
                        // one so a single bad message cannot fail the others.
                        for (envelope, reply) in envelopes.iter().zip(replies) {
                            let _ = reply.send(store.receive(std::slice::from_ref(envelope)));
                        }
                    }
                }
            }
        }
    }
}
