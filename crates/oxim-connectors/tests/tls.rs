//! MLLP and TCP over TLS and mutual TLS, with certificates generated for the
//! test.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::{Recorder, channel, delivery, destination, engine, free_port, wait_until};
use oxim_model::DataType;
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};

/// A test PKI: a CA, a server certificate for `localhost` and a client
/// certificate, written as PEM files.
struct Pki {
    _dir: tempfile::TempDir,
    ca: PathBuf,
    server_cert: PathBuf,
    server_key: PathBuf,
    client_cert: PathBuf,
    client_key: PathBuf,
}

fn make_pki() -> Pki {
    let dir = tempfile::tempdir().unwrap();
    let write = |name: &str, text: String| {
        let path = dir.path().join(name);
        std::fs::write(&path, text).unwrap();
        path
    };
    let ca_key = KeyPair::generate().unwrap();
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params
        .distinguished_name
        .push(DnType::CommonName, "OXIM Test CA");
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let ca_cert = ca_params.self_signed(&ca_key).unwrap();
    let issuer = Issuer::new(ca_params, ca_key);

    let leaf = |names: Vec<String>, usage: ExtendedKeyUsagePurpose| {
        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::new(names).unwrap();
        params.extended_key_usages = vec![usage];
        let cert = params.signed_by(&key, &issuer).unwrap();
        (cert.pem(), key.serialize_pem())
    };
    let (server_cert, server_key) = leaf(
        vec!["localhost".into()],
        ExtendedKeyUsagePurpose::ServerAuth,
    );
    let (client_cert, client_key) = leaf(
        vec!["analyzer.lab.test".into()],
        ExtendedKeyUsagePurpose::ClientAuth,
    );
    Pki {
        ca: write("ca.pem", ca_cert.pem()),
        server_cert: write("server.pem", server_cert),
        server_key: write("server-key.pem", server_key),
        client_cert: write("client.pem", client_cert),
        client_key: write("client-key.pem", client_key),
        _dir: dir,
    }
}

/// YAML for a path, with forward slashes so Windows paths need no escaping.
fn yaml_path(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\\', "/"))
}

const MESSAGE: &[u8] =
    b"MSH|^~\\&|ANALYZER|LAB|LIS|HOSP|20260929120000||ORU^R01^ORU_R01|TLS1|P|2.5.1\rPID|1||42\r";

#[tokio::test(flavor = "multi_thread")]
async fn mllp_over_mutual_tls() {
    let pki = make_pki();
    let recorder = Arc::new(Recorder::default());
    let engine = engine(recorder.clone()).await;
    let port = free_port();
    engine
        .deploy(channel(&format!(
            "  type: mllp
  data_type: hl7v2
  settings:
    listen: '127.0.0.1:{port}'
    tls:
      cert_file: {}
      key_file: {}
      client_ca_file: {}",
            yaml_path(&pki.server_cert),
            yaml_path(&pki.server_key),
            yaml_path(&pki.ca),
        )))
        .await
        .unwrap();

    let client = |with_certificate: bool| {
        let certificate = if with_certificate {
            format!(
                "\n      cert_file: {}\n      key_file: {}",
                yaml_path(&pki.client_cert),
                yaml_path(&pki.client_key)
            )
        } else {
            String::new()
        };
        destination(
            "mllp",
            &format!(
                "      target: '127.0.0.1:{port}'
      connect_timeout: 5s
      ack_timeout: 5s
      tls:
        ca_file: {}
        system_roots: false
        server_name: localhost{}",
                yaml_path(&pki.ca),
                certificate.replace("\n      ", "\n        ")
            ),
        )
    };

    // With a client certificate the message is delivered and acknowledged.
    let sender = client(true);
    let mut acknowledged = None;
    for _ in 0..100 {
        match sender.send(&delivery(1, MESSAGE, DataType::Hl7V2)).await {
            Ok(ack) => {
                acknowledged = ack;
                break;
            }
            Err(_) => tokio::time::sleep(std::time::Duration::from_millis(50)).await,
        }
    }
    let ack = String::from_utf8(acknowledged.expect("no acknowledgment")).unwrap();
    assert!(ack.contains("MSA|AA|TLS1"), "{ack}");
    wait_until("the message is stored", || recorder.payloads().len() == 1).await;
    assert_eq!(recorder.payloads()[0], MESSAGE);

    // Without one the handshake is refused and nothing is stored.
    let error = client(false)
        .send(&delivery(2, MESSAGE, DataType::Hl7V2))
        .await
        .unwrap_err();
    assert!(!error.to_string().is_empty());
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(recorder.payloads().len(), 1);

    // A server certificate from an untrusted CA is rejected by the sender.
    let other = make_pki();
    let untrusted = destination(
        "mllp",
        &format!(
            "      target: '127.0.0.1:{port}'
      tls:
        ca_file: {}
        system_roots: false
        server_name: localhost
        cert_file: {}
        key_file: {}",
            yaml_path(&other.ca),
            yaml_path(&pki.client_cert),
            yaml_path(&pki.client_key)
        ),
    );
    let error = untrusted
        .send(&delivery(3, MESSAGE, DataType::Hl7V2))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("TLS handshake"), "{error}");
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tcp_over_tls() {
    let pki = make_pki();
    let recorder = Arc::new(Recorder::default());
    let engine = engine(recorder.clone()).await;
    let port = free_port();
    engine
        .deploy(channel(&format!(
            "  type: tcp
  data_type: raw
  settings:
    listen: '127.0.0.1:{port}'
    framing: {{mode: delimited, end: '\\n'}}
    response: 'OK\\n'
    tls: {{cert_file: {}, key_file: {}}}",
            yaml_path(&pki.server_cert),
            yaml_path(&pki.server_key),
        )))
        .await
        .unwrap();
    let sender = destination(
        "tcp",
        &format!(
            "      target: '127.0.0.1:{port}'
      framing: {{mode: delimited, end: '\\n'}}
      wait_for_response: true
      expected_response: 'OK'
      tls: {{ca_file: {}, system_roots: false, server_name: localhost}}",
            yaml_path(&pki.ca)
        ),
    );
    let mut delivered = false;
    for _ in 0..100 {
        if sender
            .send(&delivery(1, b"hello over tls", DataType::Raw))
            .await
            .is_ok()
        {
            delivered = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(delivered);
    wait_until("the message is stored", || recorder.payloads().len() == 1).await;
    assert_eq!(recorder.payloads()[0], b"hello over tls");
    engine.shutdown().await;
}

#[test]
fn bad_tls_files_fail_at_deploy() {
    let config = oxim_core::ChannelConfig::from_yaml(
        "id: x
source:
  type: mllp
  data_type: hl7v2
  settings: {listen: '127.0.0.1:0', tls: {cert_file: missing.pem, key_file: missing.pem}}
",
    )
    .unwrap();
    let mut registry = oxim_core::Registry::new();
    oxim_connectors::register(&mut registry);
    let error = registry.source(&config.source).unwrap_err().to_string();
    assert!(error.contains("missing.pem"), "{error}");
}
