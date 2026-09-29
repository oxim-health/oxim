//! File source and destination in temporary directories.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::Path;
use std::sync::Arc;

use common::{Recorder, channel, delivery, destination, engine, wait_until};
use oxim_model::DataType;

fn yaml_path(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

fn files(directory: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(directory)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

#[tokio::test(flavor = "multi_thread")]
async fn source_moves_stored_files_and_rejects_oversized_ones() {
    let root = tempfile::tempdir().unwrap();
    let (inbox, processed, errors) = (
        root.path().join("in"),
        root.path().join("done"),
        root.path().join("errors"),
    );
    std::fs::create_dir_all(&inbox).unwrap();
    std::fs::write(inbox.join("b.hl7"), b"second").unwrap();
    std::fs::write(inbox.join("a.hl7"), b"first").unwrap();
    std::fs::write(inbox.join("ignored.txt"), b"not matching").unwrap();
    std::fs::write(inbox.join(".hidden.hl7"), b"hidden").unwrap();
    std::fs::write(inbox.join("huge.hl7"), vec![b'x'; 64]).unwrap();

    let recorder = Arc::new(Recorder::default());
    let engine = engine(recorder.clone()).await;
    engine
        .deploy(channel(&format!(
            "  type: file\n  data_type: raw\n  settings:\n    directory: '{}'\n    pattern: '*.HL7'\n    poll_interval: 50ms\n    processed_directory: '{}'\n    error_directory: '{}'\n    max_file_size: 32",
            yaml_path(&inbox),
            yaml_path(&processed),
            yaml_path(&errors)
        )))
        .await
        .unwrap();

    wait_until("both files are delivered", || {
        recorder.payloads().len() == 2
    })
    .await;
    // Name order.
    assert_eq!(
        recorder.payloads(),
        vec![b"first".to_vec(), b"second".to_vec()]
    );
    wait_until("stored files are moved", || {
        files(&processed) == ["a.hl7", "b.hl7"]
    })
    .await;
    wait_until("the large file is rejected", || {
        files(&errors) == ["huge.hl7"]
    })
    .await;
    assert_eq!(files(&inbox), [".hidden.hl7", "ignored.txt"]);

    // A file arriving later, with a name already in the processed directory.
    std::fs::write(inbox.join("a.hl7"), b"third").unwrap();
    wait_until("the new file is delivered", || {
        recorder.payloads().len() == 3
    })
    .await;
    wait_until("the new file is moved", || files(&processed).len() == 3).await;
    assert_eq!(files(&processed), ["a-1.hl7", "a.hl7", "b.hl7"]);
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn source_can_delete_stored_files() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("m.txt"), b"payload").unwrap();
    let recorder = Arc::new(Recorder::default());
    let engine = engine(recorder.clone()).await;
    engine
        .deploy(channel(&format!(
            "  type: file\n  data_type: raw\n  settings:\n    directory: '{}'\n    poll_interval: 50ms\n    after: delete",
            yaml_path(root.path())
        )))
        .await
        .unwrap();
    wait_until("the file is delivered", || recorder.payloads().len() == 1).await;
    wait_until("the file is deleted", || files(root.path()).is_empty()).await;
    engine.shutdown().await;
}

#[test]
fn source_requires_a_processed_directory_for_move() {
    let config = channel("  type: file\n  data_type: raw\n  settings: {directory: /tmp}");
    let mut registry = oxim_core::Registry::new();
    oxim_connectors::register(&mut registry);
    assert!(registry.source(&config.source).is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn destination_writes_files_atomically_and_idempotently() {
    let root = tempfile::tempdir().unwrap();
    let out = root.path().join("out");
    let writer = destination(
        "file",
        &format!(
            "      directory: '{}'\n      filename: '{{destination}}-{{message_id}}.{{extension}}'",
            yaml_path(&out)
        ),
    );
    let first = delivery(1, b"MSH|^~\\&|A\r", DataType::Hl7V2);
    assert_eq!(writer.send(&first).await.unwrap(), None);
    let name = format!("out-{}.hl7", first.message_id);
    assert_eq!(files(&out), std::slice::from_ref(&name));
    assert_eq!(std::fs::read(out.join(&name)).unwrap(), b"MSH|^~\\&|A\r");

    // A retry of the same delivery is harmless; different content is not.
    writer.send(&first).await.unwrap();
    let mut changed = first.clone();
    changed.payload = b"other".to_vec();
    assert!(writer.send(&changed).await.unwrap_err().permanent);
    assert_eq!(files(&out), std::slice::from_ref(&name));

    let overwriting = destination(
        "file",
        &format!(
            "      directory: '{}'\n      filename: '{{destination}}-{{message_id}}.{{extension}}'\n      overwrite: true",
            yaml_path(&out)
        ),
    );
    overwriting.send(&changed).await.unwrap();
    assert_eq!(std::fs::read(out.join(&name)).unwrap(), b"other");
    assert_eq!(files(&out), [name]);
}

#[test]
fn destination_rejects_unsafe_templates() {
    for template in ["../{message_id}", "{nope}", "a/b"] {
        let config = oxim_core::ChannelConfig::from_yaml(&format!(
            "id: lab\nsource: {{type: file, data_type: raw, settings: {{directory: x, after: delete}}}}\ndestinations:\n  - id: out\n    type: file\n    settings: {{directory: out, filename: '{template}'}}\n"
        ))
        .unwrap();
        let mut registry = oxim_core::Registry::new();
        oxim_connectors::register(&mut registry);
        assert!(
            registry.destination(&config.destinations[0]).is_err(),
            "{template}"
        );
    }
}
