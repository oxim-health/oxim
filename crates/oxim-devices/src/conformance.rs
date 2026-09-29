//! `oxim profile test`: runs a profile's fixtures through its channel in an
//! in-process engine and compares the outcome with the expectations.
//!
//! Each fixture gets a fresh engine with an in-memory store:
//!
//! - A **capture** fixture is replayed over TCP against the channel's real
//!   source, bound to a free local port. An `astm-serial` source is tested
//!   as `astm-tcp` with the same link settings, since the LIS01 link is the
//!   same on both transports; a source in client mode (OXIM connects to the
//!   device) makes the replay listen instead.
//! - A **messages** fixture submits each file directly to the channel
//!   through an in-process source, skipping the transport. HL7 v2 and ASTM
//!   files may use LF or CRLF line endings; they are converted to CR.
//!
//! Destinations are removed for the test (they would try to reach real
//! systems), unless the source relays a destination's response. The
//! outcome is then checked: message count, final status (without an
//! explicit status, no message may end in error), normalized content,
//! replies to the device, and answers the replayed device waited for.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use oxim_capture::{Capture, Endpoint, ReplayOptions};
use oxim_core::{
    ChannelConfig, Engine, EngineOptions, Registry, ResponseMode, SourceConnector, SourceContext,
    SubmitInfo, SystemClock, async_trait,
};
use oxim_model::{ChannelId, DataType, MessageStatus};
use oxim_store::{MessageQuery, MessageRecord, SqliteStore, Stage};
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::time::Instant;

use crate::profile::{Fixture, LoadedProfile, VerificationLevel};

/// The source type the runner registers for message fixtures.
pub const FIXTURE_SOURCE: &str = "oxim-profile-fixture";

/// How fixtures run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TestOptions {
    /// The longest a fixture may take.
    pub fixture_timeout: Duration,
    /// Replay timing for capture fixtures.
    pub replay: ReplayOptions,
    /// Write the actual normalized content to each fixture's
    /// `expect.normalized` file instead of comparing (for profile authors).
    pub bless: bool,
}

impl Default for TestOptions {
    fn default() -> Self {
        Self {
            fixture_timeout: Duration::from_secs(30),
            replay: ReplayOptions::default(),
            bless: false,
        }
    }
}

/// The outcome of one fixture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixtureResult {
    /// The fixture's name.
    pub name: String,
    /// Messages the channel received.
    pub messages: usize,
    /// What did not match; empty when the fixture passed.
    pub failures: Vec<String>,
}

impl FixtureResult {
    /// Whether the fixture passed.
    pub fn passed(&self) -> bool {
        self.failures.is_empty()
    }
}

/// The outcome of a profile test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConformanceReport {
    /// The profile identifier.
    pub profile: String,
    /// Vendor and model.
    pub device: String,
    /// The profile's verification level.
    pub level: VerificationLevel,
    /// One result per fixture.
    pub fixtures: Vec<FixtureResult>,
}

impl ConformanceReport {
    /// Whether the profile has fixtures and all of them passed.
    pub fn passed(&self) -> bool {
        !self.fixtures.is_empty() && self.fixtures.iter().all(FixtureResult::passed)
    }
}

impl fmt::Display for ConformanceReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "profile {} ({}), verification level {}",
            self.profile, self.device, self.level
        )?;
        for fixture in &self.fixtures {
            let messages = match fixture.messages {
                1 => "1 message".to_owned(),
                n => format!("{n} messages"),
            };
            let verdict = if fixture.passed() { "PASS" } else { "FAIL" };
            writeln!(f, "  {verdict} {} ({messages})", fixture.name)?;
            for failure in &fixture.failures {
                writeln!(f, "       - {failure}")?;
            }
        }
        let passed = self.fixtures.iter().filter(|r| r.passed()).count();
        if self.fixtures.is_empty() {
            write!(f, "no fixtures: nothing was tested")
        } else {
            write!(f, "{passed} of {} fixtures passed", self.fixtures.len())
        }
    }
}

type Request = (Vec<u8>, oneshot::Sender<Result<Option<Vec<u8>>, String>>);

/// Submits fixture messages handed to it through a queue.
#[derive(Debug)]
struct FixtureSource {
    inbox: Mutex<mpsc::Receiver<Request>>,
}

#[async_trait]
impl SourceConnector for FixtureSource {
    async fn run(&self, context: SourceContext) -> Result<(), oxim_core::ConnectorError> {
        let mut inbox = self.inbox.lock().await;
        loop {
            tokio::select! {
                () = context.cancelled() => return Ok(()),
                next = inbox.recv() => {
                    let Some((raw, answer)) = next else { return Ok(()) };
                    let outcome = if context.responds() {
                        context
                            .request(raw, SubmitInfo::default())
                            .await
                            .map(|reply| reply.data)
                    } else {
                        context.submit(raw, SubmitInfo::default()).await.map(|_| None)
                    };
                    let _ = answer.send(outcome.map_err(|e| e.to_string()));
                }
            }
        }
    }
}

/// Converts LF and CRLF line endings to CR.
fn to_cr(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut iter = bytes.iter().peekable();
    while let Some(&b) = iter.next() {
        match b {
            b'\r' => {
                if iter.peek() == Some(&&b'\n') {
                    iter.next();
                }
                out.push(b'\r');
            }
            b'\n' => out.push(b'\r'),
            other => out.push(other),
        }
    }
    out
}

/// Where the replayed device meets the channel's source.
enum ReplayTarget {
    Connect(String),
    Listen(TcpListener),
}

fn free_port() -> Result<u16, String> {
    std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .map(|address| address.port())
        .map_err(|e| format!("cannot find a free port: {e}"))
}

/// Rebinds the channel's source to a local endpoint for a capture replay.
async fn prepare_tcp(channel: &mut ChannelConfig) -> Result<ReplayTarget, String> {
    let source = &mut channel.source;
    let settings = &mut source.settings;
    if source.kind == "astm-serial" {
        // The LIS01 link runs the same over TCP.
        settings.retain(|key, _| {
            matches!(
                key.as_str(),
                "role" | "confirmation_timeout" | "send_timeout" | "max_message_len"
            )
        });
        source.kind = "astm-tcp".into();
    }
    let client_mode = matches!(source.kind.as_str(), "astm-tcp" | "astm-raw-tcp")
        && settings.get("mode").and_then(Value::as_str) == Some("client");
    match source.kind.as_str() {
        "astm-tcp" | "astm-raw-tcp" if client_mode => {
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .map_err(|e| format!("cannot listen for the channel: {e}"))?;
            let address = listener
                .local_addr()
                .map_err(|e| e.to_string())?
                .to_string();
            settings.remove("listen");
            settings.insert("connect".into(), Value::String(address));
            Ok(ReplayTarget::Listen(listener))
        }
        "astm-tcp" | "astm-raw-tcp" | "mllp" | "poct1a" | "tcp" => {
            let address = format!("127.0.0.1:{}", free_port()?);
            if matches!(source.kind.as_str(), "astm-tcp" | "astm-raw-tcp") {
                settings.remove("connect");
                settings.insert("mode".into(), Value::String("server".into()));
            }
            settings.insert("listen".into(), Value::String(address.clone()));
            Ok(ReplayTarget::Connect(address))
        }
        other => Err(format!(
            "capture fixtures need a TCP or ASTM serial source, not {other:?}; use `messages`"
        )),
    }
}

/// What a fixture run produced.
struct Outcome {
    records: Vec<MessageRecord>,
    normalized: Vec<Option<Value>>,
    replies: Vec<u8>,
    unanswered: usize,
}

async fn list(engine: &Engine, channel: &ChannelId) -> Result<Vec<MessageRecord>, String> {
    let query = MessageQuery {
        channel: Some(channel.clone()),
        limit: 1000,
        ..MessageQuery::default()
    };
    let mut records = engine
        .store()
        .run(move |store| store.list_messages(&query))
        .await
        .map_err(|e| e.to_string())?;
    records.sort_by_key(|record| record.id);
    Ok(records)
}

/// Waits until the channel has processed what it received and nothing new
/// arrives for a moment.
async fn settle(
    engine: &Engine,
    channel: &ChannelId,
    expected: Option<usize>,
    deadline: Instant,
) -> Result<Vec<MessageRecord>, String> {
    let mut previous = usize::MAX;
    let mut stable_since = Instant::now();
    loop {
        let records = list(engine, channel).await?;
        let processed = records.iter().all(|r| r.status != MessageStatus::Received);
        let enough = expected.map_or(!records.is_empty(), |n| records.len() >= n);
        if records.len() != previous {
            previous = records.len();
            stable_since = Instant::now();
        }
        if (processed && enough && stable_since.elapsed() >= Duration::from_millis(300))
            || Instant::now() >= deadline
        {
            return Ok(records);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn execute(
    loaded: &LoadedProfile,
    fixture: &Fixture,
    mut registry: Registry,
    options: &TestOptions,
) -> Result<Outcome, String> {
    let profile = &loaded.profile;
    let deadline = Instant::now() + options.fixture_timeout;
    let channel_id = ChannelId::new(profile.id.clone()).map_err(|e| e.to_string())?;
    let mut channel = profile.channel.to_channel(channel_id.clone());
    let relays = channel
        .source
        .response
        .as_ref()
        .is_some_and(|response| response.mode == ResponseMode::Destination);
    if !relays {
        channel.destinations.clear();
    }

    let mut inbox = None;
    let mut capture = None;
    let mut target = None;
    if let Some(path) = &fixture.capture {
        capture = Some(
            Capture::load(loaded.resolve(path)).map_err(|e| format!("{}: {e}", path.display()))?,
        );
        target = Some(prepare_tcp(&mut channel).await?);
    } else {
        let (tx, rx) = mpsc::channel(16);
        let source = Arc::new(FixtureSource {
            inbox: Mutex::new(rx),
        });
        registry.add_source(FIXTURE_SOURCE, move |_| {
            Ok(source.clone() as Arc<dyn SourceConnector>)
        });
        channel.source.kind = FIXTURE_SOURCE.into();
        channel.source.settings.clear();
        inbox = Some(tx);
    }

    let mut engine_options = EngineOptions::default();
    engine_options.idle_poll = Duration::from_millis(50);
    engine_options.shutdown_grace = Duration::from_secs(2);
    let store = SqliteStore::open_in_memory().map_err(|e| e.to_string())?;
    let engine = Engine::start(
        Box::new(store),
        registry,
        Arc::new(SystemClock),
        engine_options,
    )
    .await
    .map_err(|e| e.to_string())?;
    let data_type = channel.source.data_type;
    let run = async {
        engine
            .deploy(channel)
            .await
            .map_err(|e| format!("cannot deploy the profile's channel: {e}"))?;
        let mut replies = Vec::new();
        let mut unanswered = 0;
        if let (Some(capture), Some(target)) = (&capture, target) {
            let report = match target {
                ReplayTarget::Connect(address) => {
                    oxim_capture::replay(capture, &Endpoint::Connect(address), &options.replay)
                        .await
                }
                ReplayTarget::Listen(listener) => {
                    oxim_capture::replay_listening(capture, listener, &options.replay).await
                }
            }
            .map_err(|e| format!("replay failed: {e}"))?;
            replies = report.host_bytes();
            unanswered = report.unanswered;
        }
        if let Some(inbox) = &inbox {
            for path in &fixture.messages {
                let bytes = std::fs::read(loaded.resolve(path))
                    .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
                let bytes = match data_type {
                    DataType::Hl7V2 | DataType::Astm => to_cr(&bytes),
                    _ => bytes,
                };
                let (answer, answered) = oneshot::channel();
                inbox
                    .send((bytes, answer))
                    .await
                    .map_err(|_| "the fixture source stopped".to_owned())?;
                match answered.await {
                    Ok(Ok(Some(reply))) => replies.extend_from_slice(&reply),
                    Ok(Ok(None)) => {}
                    Ok(Err(e)) => return Err(format!("{}: {e}", path.display())),
                    Err(_) => return Err("the fixture source stopped".into()),
                }
            }
        }
        let records = settle(&engine, &channel_id, fixture.expect.messages, deadline).await?;
        let mut normalized = Vec::with_capacity(records.len());
        for record in &records {
            let id = record.id;
            let content = engine
                .store()
                .run(move |store| store.content(id, Stage::Normalized, None))
                .await
                .map_err(|e| e.to_string())?;
            normalized.push(content.and_then(|c| serde_json::from_slice(&c.data).ok()));
        }
        Ok(Outcome {
            records,
            normalized,
            replies,
            unanswered,
        })
    };
    let outcome = run.await;
    engine.shutdown().await;
    outcome
}

/// The first place where `actual` differs from `expected`, as a JSON
/// pointer with a description.
pub fn difference(expected: &Value, actual: &Value) -> Option<String> {
    fn walk(expected: &Value, actual: &Value, path: &mut String) -> Option<String> {
        let here = |path: &str| {
            if path.is_empty() {
                "/".to_owned()
            } else {
                path.to_owned()
            }
        };
        match (expected, actual) {
            (Value::Object(e), Value::Object(a)) => {
                for (key, value) in e {
                    let length = path.len();
                    path.push('/');
                    path.push_str(key);
                    match a.get(key) {
                        None => return Some(format!("{path}: missing (expected {value})")),
                        Some(other) => {
                            if let Some(found) = walk(value, other, path) {
                                return Some(found);
                            }
                        }
                    }
                    path.truncate(length);
                }
                a.iter()
                    .find(|(key, _)| !e.contains_key(*key))
                    .map(|(key, value)| format!("{}/{key}: unexpected {value}", path))
            }
            (Value::Array(e), Value::Array(a)) => {
                if e.len() != a.len() {
                    return Some(format!(
                        "{}: expected {} items, got {}",
                        here(path),
                        e.len(),
                        a.len()
                    ));
                }
                for (index, (value, other)) in e.iter().zip(a).enumerate() {
                    let length = path.len();
                    path.push_str(&format!("/{index}"));
                    if let Some(found) = walk(value, other, path) {
                        return Some(found);
                    }
                    path.truncate(length);
                }
                None
            }
            _ if expected == actual => None,
            _ => Some(format!("{}: expected {expected}, got {actual}", here(path))),
        }
    }
    walk(expected, actual, &mut String::new())
}

fn check(
    loaded: &LoadedProfile,
    fixture: &Fixture,
    outcome: &Outcome,
    options: &TestOptions,
) -> Vec<String> {
    let mut failures = Vec::new();
    let expect = &fixture.expect;
    let records = &outcome.records;
    if let Some(count) = expect.messages
        && records.len() != count
    {
        failures.push(format!(
            "expected {count} message(s), received {}",
            records.len()
        ));
    }
    if expect.messages.is_none() && records.is_empty() {
        failures.push("the channel received no message".into());
    }
    for (index, record) in records.iter().enumerate() {
        let error = record
            .error
            .as_deref()
            .map(|e| format!(" ({e})"))
            .unwrap_or_default();
        match expect.status {
            Some(status) if record.status != status => failures.push(format!(
                "message {}: status {}{error}, expected {}",
                index + 1,
                record.status,
                status
            )),
            None if record.status == MessageStatus::Error => {
                failures.push(format!("message {}: error{error}", index + 1));
            }
            _ => {}
        }
    }
    if let Some(path) = &expect.normalized {
        let actual = Value::Array(
            outcome
                .normalized
                .iter()
                .map(|value| value.clone().unwrap_or(Value::Null))
                .collect(),
        );
        let file = loaded.resolve(path);
        if options.bless {
            let text = serde_json::to_string_pretty(&actual).unwrap_or_default() + "\n";
            if let Err(e) = std::fs::write(&file, text) {
                failures.push(format!("cannot write {}: {e}", path.display()));
            }
        } else {
            match std::fs::read(&file)
                .map_err(|e| e.to_string())
                .and_then(|bytes| {
                    serde_json::from_slice::<Value>(&bytes).map_err(|e| e.to_string())
                }) {
                Err(e) => failures.push(format!("{}: {e}", path.display())),
                Ok(expected) => {
                    if let Some(found) = difference(&expected, &actual) {
                        failures.push(format!("normalized content at {found}"));
                    }
                }
            }
        }
    }
    let replies = String::from_utf8_lossy(&outcome.replies);
    for text in &expect.replies_contain {
        if !replies.contains(text.as_str()) {
            failures.push(format!("no reply to the device contains {text:?}"));
        }
    }
    if outcome.unanswered > 0 {
        failures.push(format!(
            "{} answer(s) the capture shows did not arrive",
            outcome.unanswered
        ));
    }
    failures
}

/// Runs every fixture of a profile. `registry` builds the component
/// registry for each fixture's engine (code tables are resolved as the
/// caller configures it, normally against the profile's directory).
pub async fn test_profile(
    loaded: &LoadedProfile,
    registry: impl Fn() -> Registry,
    options: &TestOptions,
) -> ConformanceReport {
    let profile = &loaded.profile;
    let mut fixtures = Vec::with_capacity(profile.fixtures.len());
    for fixture in &profile.fixtures {
        let result = match execute(loaded, fixture, registry(), options).await {
            Ok(outcome) => FixtureResult {
                name: fixture.name.clone(),
                messages: outcome.records.len(),
                failures: check(loaded, fixture, &outcome, options),
            },
            Err(failure) => FixtureResult {
                name: fixture.name.clone(),
                messages: 0,
                failures: vec![failure],
            },
        };
        fixtures.push(result);
    }
    ConformanceReport {
        profile: profile.id.clone(),
        device: format!("{} {}", profile.vendor, profile.model),
        level: profile.verification.level,
        fixtures,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn locates_differences() {
        let expected =
            json!({"groups": [{"observations": [{"value": "5.40"}]}], "kind": "results"});
        assert_eq!(difference(&expected, &expected), None);
        let actual = json!({"groups": [{"observations": [{"value": "5.4"}]}], "kind": "results"});
        assert_eq!(
            difference(&expected, &actual).unwrap(),
            "/groups/0/observations/0/value: expected \"5.40\", got \"5.4\""
        );
        assert_eq!(
            difference(&json!([1, 2]), &json!([1])).unwrap(),
            "/: expected 2 items, got 1"
        );
        assert_eq!(
            difference(&json!({"a": 1}), &json!({"a": 1, "b": 2})).unwrap(),
            "/b: unexpected 2"
        );
        assert_eq!(
            difference(&json!({"a": 1}), &json!({})).unwrap(),
            "/a: missing (expected 1)"
        );
    }

    #[test]
    fn converts_line_endings() {
        assert_eq!(to_cr(b"H|\r\nP|1\nL|1\r"), b"H|\rP|1\rL|1\r");
    }
}
