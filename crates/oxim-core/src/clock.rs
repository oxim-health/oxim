use std::fmt;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::SystemTime;

use oxim_model::Timestamp;

/// A source of the current time. The engine reads time only through this
/// trait so tests can control it.
pub trait Clock: Send + Sync + fmt::Debug {
    /// The current time.
    fn now(&self) -> Timestamp;
}

/// The operating system clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_system_time(SystemTime::now()).unwrap_or(Timestamp::from_unix_nanos(0))
    }
}

/// A clock that only moves when told to, for tests.
#[derive(Debug, Default)]
pub struct ManualClock(AtomicI64);

impl ManualClock {
    /// A clock showing `now`.
    pub fn new(now: Timestamp) -> Self {
        Self(AtomicI64::new(now.unix_nanos()))
    }

    /// Moves the clock to `now`.
    pub fn set(&self, now: Timestamp) {
        self.0.store(now.unix_nanos(), Ordering::SeqCst);
    }

    /// Moves the clock forward.
    pub fn advance(&self, by: std::time::Duration) {
        let nanos = i64::try_from(by.as_nanos()).unwrap_or(i64::MAX);
        self.0.fetch_add(nanos, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_unix_nanos(self.0.load(Ordering::SeqCst))
    }
}
