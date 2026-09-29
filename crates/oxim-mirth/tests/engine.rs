//! An imported Mirth channel runs in the engine with the real connectors:
//! an HL7 message sent over MLLP is filtered, mapped and archived as a file.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use oxim_core::{ChannelConfig, Engine, EngineOptions, Registry, SystemClock};
use oxim_mirth::{ImportOptions, import};
use oxim_store::SqliteStore;
use oxim_transform::TransformEnvironment;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

async fn engine() -> Engine {
    let mut registry = Registry::new();
    oxim_connectors::register(&mut registry);
    oxim_transform::register(&mut registry, TransformEnvironment::in_memory());
    let mut options = EngineOptions::default();
    options.idle_poll = Duration::from_millis(20);
    options.shutdown_grace = Duration::from_secs(2);
    Engine::start(
        Box::new(SqliteStore::open_in_memory().unwrap()),
        registry,
        Arc::new(SystemClock),
        options,
    )
    .await
    .unwrap()
}

/// Sends one MLLP frame and returns the acknowledgment.
async fn send(port: u16, message: &str) -> String {
    let mut stream = None;
    for _ in 0..100 {
        match TcpStream::connect(("127.0.0.1", port)).await {
            Ok(connected) => {
                stream = Some(connected);
                break;
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
        }
    }
    let mut stream = stream.expect("the MLLP source did not start");
    let mut frame = vec![0x0b];
    frame.extend_from_slice(message.as_bytes());
    frame.extend_from_slice(&[0x1c, 0x0d]);
    stream.write_all(&frame).await.unwrap();
    let mut reply = Vec::new();
    let mut buffer = [0u8; 1024];
    while !reply.ends_with(&[0x1c, 0x0d]) {
        let read = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut buffer))
            .await
            .expect("no acknowledgment")
            .unwrap();
        assert!(read > 0, "connection closed");
        reply.extend_from_slice(&buffer[..read]);
    }
    String::from_utf8_lossy(&reply).into_owned()
}

fn archived(directory: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut files: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
        .map(|entry| std::fs::read_to_string(entry.path()).unwrap())
        .collect();
    files.sort();
    files
}

#[tokio::test(flavor = "multi_thread")]
async fn an_imported_mllp_to_file_channel_runs() {
    let archive = tempfile::tempdir().unwrap();
    let port = free_port();
    let export = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/adt-archive.xml"),
    )
    .unwrap();
    let options = ImportOptions::new()
        .with_value("mllp_port", port.to_string())
        .with_value("archive_dir", archive.path().display().to_string());
    let result = import(&export, &options).unwrap();
    let channel = ChannelConfig::from_yaml(&result.channels[0].yaml).unwrap();

    let engine = engine().await;
    engine.deploy(channel).await.unwrap();

    // Accepted: an ADT message with a patient identifier.
    let ack = send(
        port,
        "MSH|^~\\&|HIS|HOSP|RIS|HOSP|20260929101500||ADT^A01|MSG0001|P|2.5\r\
         PID|1||PAT0042^^^HOSP^MR||DOE^JANE||19800101\r",
    )
    .await;
    assert!(ack.contains("MSA|AA|MSG0001"), "{ack}");
    // Filtered: a test system, and an order message.
    for (control, message) in [
        (
            "MSG0002",
            "MSH|^~\\&|TESTHIS|HOSP|RIS|HOSP|20260929101600||ADT^A01|MSG0002|P|2.5\rPID|1||PAT0043\r",
        ),
        (
            "MSG0003",
            "MSH|^~\\&|HIS|HOSP|RIS|HOSP|20260929101700||ORM^O01|MSG0003|P|2.5\rPID|1||PAT0044\r",
        ),
    ] {
        let ack = send(port, message).await;
        assert!(ack.contains(&format!("MSA|AA|{control}")), "{ack}");
    }

    let mut files = Vec::new();
    for _ in 0..300 {
        files = archived(archive.path());
        if !files.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // Give filtered messages time to show up if they were wrongly delivered.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let files_after = archived(archive.path());
    assert_eq!(files_after.len(), 1, "{files_after:?}");
    let file = &files[0];
    assert!(
        file.starts_with("MSH|^~\\&|HIS|HOSP|RIS|ARCHIVE|"),
        "{file}"
    );
    assert!(
        file.contains("\rPID|1|PAT0042|PAT0042^^^HOSP^MR||DOE^JANE||19800101|U"),
        "{file}"
    );
    engine.shutdown().await;
}
