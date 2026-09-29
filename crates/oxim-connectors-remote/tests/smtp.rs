//! The SMTP destination against a small in-process SMTP server with
//! STARTTLS and AUTH PLAIN.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::{Arc, Mutex};

use base64::Engine as _;
use common::{delivery, destination, yaml_path};
use oxim_model::DataType;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;

const PASSWORD: &str = "synthetic-smtp-password";

#[derive(Default)]
struct Mailbox {
    messages: Mutex<Vec<String>>,
    /// Whether every AUTH happened after STARTTLS.
    auth_encrypted: Mutex<Vec<bool>>,
}

enum Outcome {
    Done,
    StartTls,
}

async fn send<S: AsyncWrite + Unpin>(out: &mut S, text: &str) {
    out.write_all(text.as_bytes()).await.unwrap();
    out.flush().await.unwrap();
}

async fn commands<S: AsyncRead + AsyncWrite + Unpin>(
    control: &mut BufReader<S>,
    secure: bool,
    tls_available: bool,
    mailbox: &Mailbox,
) -> Outcome {
    loop {
        let mut line = String::new();
        if control.read_line(&mut line).await.unwrap_or(0) == 0 {
            return Outcome::Done;
        }
        let upper = line.to_ascii_uppercase();
        let out = control.get_mut();
        if upper.starts_with("EHLO") || upper.starts_with("HELO") {
            if tls_available && !secure {
                send(
                    out,
                    "250-synthetic.test\r\n250-STARTTLS\r\n250 AUTH PLAIN\r\n",
                )
                .await;
            } else {
                send(out, "250-synthetic.test\r\n250 AUTH PLAIN\r\n").await;
            }
        } else if upper.starts_with("STARTTLS") {
            send(out, "220 Ready to start TLS\r\n").await;
            return Outcome::StartTls;
        } else if upper.starts_with("AUTH PLAIN") {
            let encoded = line.trim_end().split(' ').nth(2).unwrap_or_default();
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .unwrap_or_default();
            mailbox.auth_encrypted.lock().unwrap().push(secure);
            if decoded == format!("\0lab\0{PASSWORD}").as_bytes() {
                send(out, "235 2.7.0 Authentication successful\r\n").await;
            } else {
                send(out, "535 5.7.8 Authentication credentials invalid\r\n").await;
            }
        } else if upper.starts_with("MAIL FROM")
            || upper.starts_with("RSET")
            || upper.starts_with("NOOP")
        {
            send(out, "250 2.1.0 OK\r\n").await;
        } else if upper.starts_with("RCPT TO") {
            if upper.contains("UNKNOWN@") {
                send(out, "550 5.1.1 No such user\r\n").await;
            } else {
                send(out, "250 2.1.5 OK\r\n").await;
            }
        } else if upper.starts_with("DATA") {
            send(out, "354 End data with <CR><LF>.<CR><LF>\r\n").await;
            let mut message = String::new();
            loop {
                let mut data = String::new();
                if control.read_line(&mut data).await.unwrap_or(0) == 0 {
                    return Outcome::Done;
                }
                if data == ".\r\n" {
                    break;
                }
                message.push_str(&data);
            }
            mailbox.messages.lock().unwrap().push(message);
            send(control.get_mut(), "250 2.0.0 Queued as SYNTH1\r\n").await;
        } else if upper.starts_with("QUIT") {
            send(out, "221 2.0.0 Bye\r\n").await;
            return Outcome::Done;
        } else {
            send(out, "502 5.5.2 Command not recognized\r\n").await;
        }
    }
}

async fn session(socket: TcpStream, tls: Option<TlsAcceptor>, mailbox: Arc<Mailbox>) {
    let mut control = BufReader::new(socket);
    send(control.get_mut(), "220 synthetic.test ESMTP\r\n").await;
    if let Outcome::StartTls = commands(&mut control, false, tls.is_some(), &mailbox).await
        && let Some(acceptor) = tls
        && let Ok(stream) = acceptor.accept(control.into_inner()).await
    {
        let mut secure = BufReader::new(stream);
        commands(&mut secure, true, true, &mailbox).await;
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    port: u16,
    ca: std::path::PathBuf,
    mailbox: Arc<Mailbox>,
}

async fn start(tls: bool) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let key = rcgen::KeyPair::generate().unwrap();
    let certificate = rcgen::CertificateParams::new(vec!["localhost".to_owned()])
        .unwrap()
        .self_signed(&key)
        .unwrap();
    let ca = dir.path().join("ca.pem");
    std::fs::write(&ca, certificate.pem()).unwrap();
    let acceptor = tls.then(|| {
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![certificate.der().clone()],
            rustls_pki_types::PrivateKeyDer::try_from(key.serialize_der()).unwrap(),
        )
        .unwrap();
        TlsAcceptor::from(Arc::new(config))
    });
    let mailbox = Arc::new(Mailbox::default());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let shared = mailbox.clone();
    tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(session(socket, acceptor.clone(), shared.clone()));
        }
    });
    Fixture {
        _dir: dir,
        port,
        ca,
        mailbox,
    }
}

const RESULT: &[u8] =
    b"MSH|^~\\&|ANALYZER|LAB|LIS|HOSP|20260929120000||ORU^R01|M1|P|2.5.1\rPID|1||SYNTH-4\r";

#[tokio::test(flavor = "multi_thread")]
async fn sends_attachments_over_starttls_with_authentication() {
    let fixture = start(true).await;
    let sender = destination(
        "smtp",
        &format!(
            "      host: 127.0.0.1
      port: {}
      tls: {{ca_file: {}, system_roots: false, server_name: localhost}}
      username: lab
      password: {PASSWORD}
      from: 'OXIM <oxim@lab.test>'
      to: [results@lab.test]
      cc: [archive@lab.test]
      subject: 'Result {{message_id}}'
      hello_name: oxim.lab.test",
            fixture.port,
            yaml_path(&fixture.ca)
        ),
    );
    let answer = sender
        .send(&delivery(1, RESULT, DataType::Hl7V2))
        .await
        .unwrap()
        .unwrap();
    assert!(String::from_utf8(answer).unwrap().starts_with("250"));
    let messages = fixture.mailbox.messages.lock().unwrap().clone();
    assert_eq!(messages.len(), 1);
    let message = &messages[0];
    let id = delivery(1, RESULT, DataType::Hl7V2).message_id;
    assert!(
        message.contains(&format!("Subject: Result {id}")),
        "{message}"
    );
    assert!(
        message.contains(&format!("Message-ID: <{id}.out@oxim.invalid>")),
        "{message}"
    );
    assert!(message.contains("Cc: archive@lab.test"), "{message}");
    assert!(message.contains(&format!("\"lab-{id}.hl7\"")), "{message}");
    assert!(message.contains("x-application/hl7-v2+er7"), "{message}");
    // Credentials were only sent after STARTTLS.
    assert_eq!(*fixture.mailbox.auth_encrypted.lock().unwrap(), [true]);
}

#[tokio::test(flavor = "multi_thread")]
async fn classifies_failures() {
    let fixture = start(false).await;
    let settings = |to: &str, password: &str| {
        format!(
            "      host: 127.0.0.1
      port: {}
      security: none
      username: lab
      password: {password}
      from: oxim@lab.test
      to: [{to}]
      content: body",
            fixture.port
        )
    };
    let ok = destination("smtp", &settings("results@lab.test", PASSWORD));
    ok.send(&delivery(1, RESULT, DataType::Hl7V2))
        .await
        .unwrap();
    let body = fixture.mailbox.messages.lock().unwrap()[0].clone();
    assert!(body.contains("SYNTH-4"), "{body}");

    let unknown = destination("smtp", &settings("unknown@lab.test", PASSWORD));
    let error = unknown
        .send(&delivery(2, RESULT, DataType::Hl7V2))
        .await
        .unwrap_err();
    assert!(error.permanent, "{error}");

    let wrong = destination("smtp", &settings("results@lab.test", "wrong"));
    let error = wrong
        .send(&delivery(3, RESULT, DataType::Hl7V2))
        .await
        .unwrap_err();
    assert!(!error.permanent, "{error}");

    let binary = destination("smtp", &settings("results@lab.test", PASSWORD));
    let error = binary
        .send(&delivery(4, &[0xff, 0xfe, 0x00], DataType::Raw))
        .await
        .unwrap_err();
    assert!(
        error.permanent && error.to_string().contains("UTF-8"),
        "{error}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn starttls_is_required() {
    // A server without STARTTLS is refused instead of used in plain text.
    let fixture = start(false).await;
    let sender = destination(
        "smtp",
        &format!(
            "      host: 127.0.0.1
      port: {}
      username: lab
      password: {PASSWORD}
      from: oxim@lab.test
      to: [results@lab.test]",
            fixture.port
        ),
    );
    let error = sender
        .send(&delivery(1, RESULT, DataType::Hl7V2))
        .await
        .unwrap_err();
    assert!(!error.to_string().is_empty());
    assert!(fixture.mailbox.messages.lock().unwrap().is_empty());
    assert!(fixture.mailbox.auth_encrypted.lock().unwrap().is_empty());
}
