//! Login throttling.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy)]
struct Entry {
    window_start: Instant,
    failures: u32,
}

/// Limits failed logins per user name and per client address.
///
/// Each key may fail `max_failures` times within `window`; further
/// attempts are refused until the window ends. State is in memory and
/// bounded: when `max_entries` keys are tracked, the oldest are dropped.
#[derive(Debug)]
pub struct LoginThrottle {
    entries: Mutex<HashMap<String, Entry>>,
    max_failures: u32,
    window: Duration,
    max_entries: usize,
}

impl Default for LoginThrottle {
    /// 10 failures per 15 minutes, 10 000 tracked keys.
    fn default() -> Self {
        Self::new(10, Duration::from_secs(15 * 60), 10_000)
    }
}

impl LoginThrottle {
    /// Creates a throttle.
    pub fn new(max_failures: u32, window: Duration, max_entries: usize) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            max_failures: max_failures.max(1),
            window,
            max_entries: max_entries.max(1),
        }
    }

    /// The throttle keys of a login attempt.
    pub fn keys(username: &str, address: Option<&str>) -> Vec<String> {
        let mut keys = vec![format!("user:{}", username.to_lowercase())];
        if let Some(address) = address {
            keys.push(format!("addr:{address}"));
        }
        keys
    }

    /// Returns how long to wait if any key is blocked.
    pub fn check(&self, keys: &[String], now: Instant) -> Result<(), Duration> {
        let Ok(entries) = self.entries.lock() else {
            return Ok(());
        };
        let mut wait = Duration::ZERO;
        for key in keys {
            if let Some(entry) = entries.get(key) {
                let elapsed = now.saturating_duration_since(entry.window_start);
                if elapsed < self.window && entry.failures >= self.max_failures {
                    wait = wait.max(self.window - elapsed);
                }
            }
        }
        if wait.is_zero() { Ok(()) } else { Err(wait) }
    }

    /// Counts a failed attempt against every key.
    pub fn record_failure(&self, keys: &[String], now: Instant) {
        let Ok(mut entries) = self.entries.lock() else {
            return;
        };
        if entries.len() + keys.len() > self.max_entries {
            let window = self.window;
            entries.retain(|_, entry| now.saturating_duration_since(entry.window_start) < window);
            while entries.len() + keys.len() > self.max_entries {
                let oldest = entries
                    .iter()
                    .min_by_key(|(_, entry)| entry.window_start)
                    .map(|(key, _)| key.clone());
                match oldest {
                    Some(key) => {
                        entries.remove(&key);
                    }
                    None => break,
                }
            }
        }
        for key in keys {
            let entry = entries.entry(key.clone()).or_insert(Entry {
                window_start: now,
                failures: 0,
            });
            if now.saturating_duration_since(entry.window_start) >= self.window {
                *entry = Entry {
                    window_start: now,
                    failures: 0,
                };
            }
            entry.failures = entry.failures.saturating_add(1);
        }
    }

    /// Clears the user name key after a successful login. The address key
    /// is kept so one address cannot probe many accounts.
    pub fn record_success(&self, username: &str) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.remove(&format!("user:{}", username.to_lowercase()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_after_too_many_failures() {
        let throttle = LoginThrottle::new(3, Duration::from_secs(60), 100);
        let start = Instant::now();
        let keys = LoginThrottle::keys("Alice", Some("10.0.0.1"));
        for _ in 0..3 {
            assert!(throttle.check(&keys, start).is_ok());
            throttle.record_failure(&keys, start);
        }
        let wait = throttle
            .check(&keys, start + Duration::from_secs(10))
            .unwrap_err();
        assert_eq!(wait, Duration::from_secs(50));
        // Another user from the same address is blocked too.
        let other = LoginThrottle::keys("bob", Some("10.0.0.1"));
        assert!(throttle.check(&other, start).is_err());
        // The window ends.
        assert!(
            throttle
                .check(&keys, start + Duration::from_secs(61))
                .is_ok()
        );
        throttle.record_success("alice");
        assert!(
            throttle
                .check(&LoginThrottle::keys("alice", None), start)
                .is_ok()
        );
    }

    #[test]
    fn stays_bounded() {
        let throttle = LoginThrottle::new(1, Duration::from_secs(60), 10);
        let now = Instant::now();
        for i in 0..100 {
            throttle.record_failure(&[format!("user:{i}")], now);
        }
        assert!(throttle.entries.lock().unwrap().len() <= 10);
    }
}
