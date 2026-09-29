//! The example profiles in `profiles/` load and pass their fixtures.
//!
//! The captures were recorded with the ignored `record_example_captures`
//! test: it runs each profile's channel, puts the `oxim-capture` proxy in
//! front of it and drives an `oxim-sim` device through the proxy. Run it
//! again after changing a simulator or a profile:
//!
//! ```text
//! cargo test -p oxim-devices --test profiles -- --ignored
//! ```

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use oxim_capture::{CaptureSink, DeviceSide, Header, Protocol, TcpProxy, Transport, now};
use oxim_core::{Engine, EngineOptions, Registry, SystemClock};
use oxim_devices::{DeviceEnvironment, LoadedProfile, Profile, TestOptions, test_profile};
use oxim_lab::LabEnvironment;
use oxim_model::ChannelId;
use oxim_store::SqliteStore;
use oxim_transform::TransformEnvironment;
use tokio::net::TcpStream;
use tokio::sync::oneshot;

fn profiles_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles")
}

fn registry(tables: &Path, data: &Path) -> Registry {
    let mut registry = Registry::new();
    oxim_connectors::register(&mut registry);
    oxim_mapping::register(&mut registry);
    oxim_transform::register(&mut registry, TransformEnvironment::new(tables));
    oxim_lab::register(
        &mut registry,
        LabEnvironment::new(data.join("orders.db"), tables),
    );
    oxim_devices::register(
        &mut registry,
        DeviceEnvironment::new(data.join("devices.db")),
    );
    registry
}

const PROFILES: [&str; 3] = [
    "generic-astm-analyzer",
    "generic-ihe-law-analyzer",
    "generic-poct1a-device",
];

#[tokio::test(flavor = "multi_thread")]
async fn example_profiles_pass_their_fixtures() {
    for name in PROFILES {
        let loaded = Profile::load(profiles_dir().join(name).join("profile.yaml"))
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let data = tempfile::tempdir().unwrap();
        let tables = loaded.dir.clone();
        let report = test_profile(
            &loaded,
            || registry(&tables, data.path()),
            &TestOptions::default(),
        )
        .await;
        assert!(report.passed(), "{report}");
        assert!(report.to_string().ends_with("fixtures passed"), "{report}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failing_expectation_is_reported() {
    let source = profiles_dir().join("generic-astm-analyzer");
    let dir = tempfile::tempdir().unwrap();
    for sub in ["fixtures", "tables"] {
        std::fs::create_dir(dir.path().join(sub)).unwrap();
        for entry in std::fs::read_dir(source.join(sub)).unwrap() {
            let entry = entry.unwrap();
            std::fs::copy(entry.path(), dir.path().join(sub).join(entry.file_name())).unwrap();
        }
    }
    let text = std::fs::read_to_string(source.join("profile.yaml"))
        .unwrap()
        .replace("      messages: 2\n", "      messages: 3\n")
        .replace("'L|1|I'", "'L|1|N'");
    let path = dir.path().join("profile.yaml");
    // `messages: 3` disagrees with the expected content file.
    std::fs::write(&path, &text).unwrap();
    assert!(Profile::load(&path).is_err());
    let expected = dir.path().join("fixtures/results.expected.json");
    let mut values: Vec<serde_json::Value> =
        serde_json::from_slice(&std::fs::read(&expected).unwrap()).unwrap();
    values.push(values[0].clone());
    std::fs::write(&expected, serde_json::to_vec(&values).unwrap()).unwrap();
    let loaded = Profile::load(&path).unwrap();

    let data = tempfile::tempdir().unwrap();
    let tables = loaded.dir.clone();
    let options = TestOptions {
        fixture_timeout: Duration::from_secs(5),
        ..TestOptions::default()
    };
    let report = test_profile(&loaded, || registry(&tables, data.path()), &options).await;
    assert!(!report.passed());
    let text = report.to_string();
    assert!(
        text.contains("FAIL two-result-messages (2 messages)"),
        "{text}"
    );
    assert!(text.contains("expected 3 message(s), received 2"), "{text}");
    assert!(
        text.contains("normalized content at /: expected 3 items, got 2"),
        "{text}"
    );
    assert!(
        text.contains("no reply to the device contains \"L|1|N\""),
        "{text}"
    );
    assert!(text.ends_with("0 of 2 fixtures passed"), "{text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn anonymized_captures_still_replay() {
    for (name, capture) in [
        ("generic-astm-analyzer", "fixtures/results.oximcap"),
        ("generic-ihe-law-analyzer", "fixtures/results.oximcap"),
        ("generic-poct1a-device", "fixtures/upload.oximcap"),
    ] {
        let source = profiles_dir().join(name);
        let dir = tempfile::tempdir().unwrap();
        for sub in ["fixtures", "tables"] {
            std::fs::create_dir(dir.path().join(sub)).unwrap();
            for entry in std::fs::read_dir(source.join(sub)).unwrap() {
                let entry = entry.unwrap();
                std::fs::copy(entry.path(), dir.path().join(sub).join(entry.file_name())).unwrap();
            }
        }
        // Anonymized values differ from the expected normalized content, so
        // only the message count, statuses and replies are compared.
        let text: String = std::fs::read_to_string(source.join("profile.yaml"))
            .unwrap()
            .lines()
            .filter(|line| !line.trim_start().starts_with("normalized:"))
            .map(|line| format!("{line}\n"))
            .collect();
        std::fs::write(dir.path().join("profile.yaml"), text).unwrap();
        let path = dir.path().join(capture);
        let original = oxim_capture::Capture::load(&path).unwrap();
        let anonymizer =
            oxim_anonymize::Anonymizer::new(oxim_anonymize::Options::default(), None, None)
                .unwrap();
        let mut report = anonymizer.report();
        let anonymized =
            oxim_anonymize::capture::anonymize_capture(&anonymizer, &original, &mut report);
        assert!(
            report.total() > 0 && report.warnings.is_empty(),
            "{name}: {report}"
        );
        anonymized.save(&path).unwrap();

        let loaded = Profile::load(dir.path().join("profile.yaml")).unwrap();
        let data = tempfile::tempdir().unwrap();
        let tables = loaded.dir.clone();
        let result = test_profile(
            &loaded,
            || registry(&tables, data.path()),
            &TestOptions::default(),
        )
        .await;
        assert!(result.passed(), "{name}: {result}");
    }
}

/// Runs the profile's channel with the capture proxy in front of it and
/// `drive` as the device, and writes the capture.
async fn record<F, Fut>(
    loaded: &LoadedProfile,
    protocol: Protocol,
    capture: &str,
    description: &str,
    drive: F,
) where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let data = tempfile::tempdir().unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let target = format!("127.0.0.1:{port}");
    let mut channel = loaded
        .profile
        .channel
        .to_channel(ChannelId::new(loaded.profile.id.clone()).unwrap());
    channel.destinations.clear();
    channel
        .source
        .settings
        .insert("listen".into(), serde_json::Value::String(target.clone()));
    let mut options = EngineOptions::default();
    options.idle_poll = Duration::from_millis(50);
    let engine = Engine::start(
        Box::new(SqliteStore::open_in_memory().unwrap()),
        registry(&loaded.dir, data.path()),
        Arc::new(SystemClock),
        options,
    )
    .await
    .unwrap();
    engine.deploy(channel).await.unwrap();

    let path = loaded.dir.join(capture);
    let mut header = Header::new(Transport::Tcp, now());
    header.tool = Some("oxim-capture (oxim-devices example recording)".into());
    header.protocol = Some(protocol);
    header.description = Some(description.into());
    header.device = Some("oxim-sim".into());
    header.host = Some("OXIM".into());
    let sink = CaptureSink::new(std::fs::File::create(&path).unwrap(), &header).unwrap();
    let proxy = TcpProxy::bind("127.0.0.1:0", target.clone(), DeviceSide::Client)
        .await
        .unwrap();
    let address = proxy.local_addr().unwrap().to_string();
    let (stop, stopped) = oneshot::channel::<()>();
    let proxy_task = tokio::spawn(proxy.run(sink.clone(), async {
        let _ = stopped.await;
    }));
    // Wait until the channel listens.
    for _ in 0..100 {
        if TcpStream::connect(&target).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    drive(address).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let _ = stop.send(());
    proxy_task.await.unwrap().unwrap();
    engine.shutdown().await;
}

const LAW_RESULTS: &str = "MSH|^~\\&|LAW-ANALYZER|LAB|OXIM|LAB|20260929120000||OUL^R22^OUL_R22|LAW000001|P|2.5.1\r\
PID|1||PAT000101^^^LAB^MR||Test^Robin||19800101|U\r\
SPM|1|SMP000101||SER\r\
OBR|1|ORD000101||GLU^Glucose^L\r\
OBX|1|NM|GLU^Glucose^L||5.40|mmol/L|3.9-6.1|N|||F|||20260929115900\r\
SPM|2|SMP000102||BLD\r\
OBR|2|ORD000102||HGB^Hemoglobin^L\r\
OBX|1|NM|HGB^Hemoglobin^L||13.8|g/dL|12.0-17.0|N|||F|||20260929115900\r\
OBX|2|NM|WBC^Leukocytes^L||6.25|10*9/L|4.00-10.00|N|||F|||20260929115900\r";

/// Records the example captures and blesses the expected outputs.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "rewrites the example fixtures"]
async fn record_example_captures() {
    let load = |name: &str| {
        let path = profiles_dir().join(name).join("profile.yaml");
        let text = std::fs::read_to_string(&path).unwrap();
        let dir = path.parent().unwrap().to_owned();
        LoadedProfile {
            profile: Profile::from_yaml(&text).unwrap(),
            path,
            dir,
        }
    };

    let astm = load("generic-astm-analyzer");
    record(
        &astm,
        Protocol::AstmLis01,
        "fixtures/results.oximcap",
        "Two synthetic result messages from the oxim-sim ASTM analyzer.",
        |address| async move {
            let mut generator = oxim_sim::generate::Generator::new(11);
            let messages = vec![generator.astm_results(3), generator.astm_results(2)];
            let stream = TcpStream::connect(address).await.unwrap();
            let report = oxim_sim::astm::send(stream, messages, |_| {})
                .await
                .unwrap();
            assert_eq!(report.delivered, 2);
        },
    )
    .await;

    let law = load("generic-ihe-law-analyzer");
    record(
        &law,
        Protocol::Hl7v2Mllp,
        "fixtures/results.oximcap",
        "One synthetic OUL^R22 with two specimens.",
        |address| async move {
            let report = oxim_sim::mllp::send(
                &address,
                vec![LAW_RESULTS.as_bytes().to_vec()],
                None,
                Duration::from_secs(10),
                |_, _| {},
            )
            .await
            .unwrap();
            assert_eq!(report.accepted, 1);
        },
    )
    .await;

    let poct = load("generic-poct1a-device");
    record(
        &poct,
        Protocol::Poct1a,
        "fixtures/upload.oximcap",
        "A synthetic oxim-sim POCT1-A conversation with two observations.",
        |address| async move {
            oxim_sim::poct::send(&address, 2, Duration::from_secs(10), |_| {})
                .await
                .unwrap();
        },
    )
    .await;

    for loaded in [astm, law, poct] {
        let data = tempfile::tempdir().unwrap();
        let tables = loaded.dir.clone();
        let bless = TestOptions {
            bless: true,
            ..TestOptions::default()
        };
        let report = test_profile(&loaded, || registry(&tables, data.path()), &bless).await;
        assert!(report.passed(), "{report}");
    }
}
