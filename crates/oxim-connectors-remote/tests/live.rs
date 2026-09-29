//! Optional checks against real services. Each test runs only when its
//! environment variable holds the destination settings as a YAML flow
//! mapping, and is skipped otherwise:
//!
//! | Variable | Example |
//! |---|---|
//! | `OXIM_LIVE_SFTP` | `{host: files.lab.internal, username: oxim, password_env: SFTP_PASSWORD, known_hosts_file: /home/oxim/.ssh/known_hosts, directory: oxim-live}` |
//! | `OXIM_LIVE_FTP` | `{host: ftp.lab.internal, username: oxim, password_env: FTP_PASSWORD, directory: oxim-live}` |
//! | `OXIM_LIVE_S3` | `{endpoint: 'https://s3.eu-central-1.amazonaws.com', region: eu-central-1, bucket: oxim-live, access_key_id_env: AWS_ACCESS_KEY_ID, secret_access_key_env: AWS_SECRET_ACCESS_KEY, directory: live}` |
//! | `OXIM_LIVE_SMTP` | `{host: smtp.lab.internal, username: oxim, password_env: SMTP_PASSWORD, from: oxim@lab.internal, to: [integration@lab.internal]}` |
//! | `OXIM_LIVE_SOAP` | `{url: 'https://ws.lab.internal/echo', version: '1.2'}` |
//!
//! Every test sends one synthetic message; file-like destinations send it
//! twice to check that a repeated delivery is accepted.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::delivery;
use oxim_model::DataType;

const SYNTHETIC: &[u8] =
    b"MSH|^~\\&|OXIM|LIVE|TEST|LIVE|20260929120000||ORU^R01|LIVE1|T|2.5.1\rPID|1||SYNTHETIC-LIVE\r";

async fn live(kind: &str, variable: &str, twice: bool) {
    let Ok(settings) = std::env::var(variable) else {
        eprintln!("{variable} is not set; skipping the live {kind} test");
        return;
    };
    let sender = destination_from_flow(kind, &settings);
    let message = delivery(u64::from(fastrand::u32(..)), SYNTHETIC, DataType::Hl7V2);
    sender.send(&message).await.unwrap();
    if twice {
        sender.send(&message).await.unwrap();
    }
}

fn destination_from_flow(
    kind: &str,
    flow: &str,
) -> std::sync::Arc<dyn oxim_core::DestinationConnector> {
    let config = oxim_core::ChannelConfig::from_yaml(&format!(
        "id: live\nsource: {{type: timer, data_type: raw, settings: {{interval: 1h}}}}\ndestinations:\n  - id: out\n    type: {kind}\n    settings: {}\n",
        flow.trim()
    ))
    .unwrap();
    common::registry(std::sync::Arc::new(common::Recorder::default()))
        .destination(&config.destinations[0])
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn live_sftp() {
    live("sftp", "OXIM_LIVE_SFTP", true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn live_ftp() {
    live("ftp", "OXIM_LIVE_FTP", true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn live_s3() {
    live("s3", "OXIM_LIVE_S3", true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn live_smtp() {
    live("smtp", "OXIM_LIVE_SMTP", false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn live_soap() {
    live("soap", "OXIM_LIVE_SOAP", false).await;
}
