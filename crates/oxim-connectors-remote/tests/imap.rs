//! The IMAP source against a small in-process IMAP server.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{Recorder, channel, engine, wait_until, yaml_path};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

const PASSWORD: &str = "synthetic-imap-password";

#[derive(Debug, Clone)]
struct Email {
    uid: u32,
    data: Vec<u8>,
    seen: bool,
    deleted: bool,
    folder: String,
}

#[derive(Default)]
struct Server {
    emails: Mutex<Vec<Email>>,
}

/// The first quoted or bare arguments of a command.
fn arguments(rest: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = rest.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c == ' ' {
            chars.next();
        } else if c == '"' {
            chars.next();
            let mut value = String::new();
            while let Some(c) = chars.next() {
                match c {
                    '\\' => value.extend(chars.next()),
                    '"' => break,
                    other => value.push(other),
                }
            }
            out.push(value);
        } else {
            let mut value = String::new();
            while let Some(&c) = chars.peek() {
                if c == ' ' {
                    break;
                }
                value.push(c);
                chars.next();
            }
            out.push(value);
        }
    }
    out
}

async fn session<S: AsyncRead + AsyncWrite + Unpin>(stream: S, server: Arc<Server>) {
    let mut control = BufReader::new(stream);
    let write = |text: String| text.into_bytes();
    control
        .get_mut()
        .write_all(b"* OK [CAPABILITY IMAP4rev1 MOVE] synthetic IMAP ready\r\n")
        .await
        .unwrap();
    loop {
        let mut line = String::new();
        if control.read_line(&mut line).await.unwrap_or(0) == 0 {
            return;
        }
        let line = line.trim_end();
        let (tag, rest) = line.split_once(' ').unwrap_or((line, ""));
        let (command, rest) = rest.split_once(' ').unwrap_or((rest, ""));
        let mut out: Vec<u8> = Vec::new();
        match command.to_ascii_uppercase().as_str() {
            "CAPABILITY" => {
                out.extend(write(format!(
                    "* CAPABILITY IMAP4rev1 MOVE\r\n{tag} OK done\r\n"
                )));
            }
            "LOGIN" => {
                let args = arguments(rest);
                if args.len() == 2 && args[0] == "lab" && args[1] == PASSWORD {
                    out.extend(write(format!("{tag} OK LOGIN completed\r\n")));
                } else {
                    out.extend(write(format!(
                        "{tag} NO [AUTHENTICATIONFAILED] Invalid credentials\r\n"
                    )));
                }
            }
            "SELECT" => {
                let count = server
                    .emails
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|e| e.folder == "INBOX")
                    .count();
                out.extend(write(format!(
                    "* FLAGS (\\Seen \\Deleted)\r\n* {count} EXISTS\r\n* 0 RECENT\r\n* OK [UIDVALIDITY 1] UIDs valid\r\n{tag} OK [READ-WRITE] SELECT completed\r\n"
                )));
            }
            "UID" => {
                let (sub, rest) = rest.split_once(' ').unwrap_or((rest, ""));
                let mut emails = server.emails.lock().unwrap();
                match sub.to_ascii_uppercase().as_str() {
                    "SEARCH" => {
                        let unseen = rest.eq_ignore_ascii_case("UNSEEN");
                        let uids: Vec<String> = emails
                            .iter()
                            .filter(|e| e.folder == "INBOX" && !e.deleted && (!unseen || !e.seen))
                            .map(|e| e.uid.to_string())
                            .collect();
                        let list = if uids.is_empty() {
                            String::new()
                        } else {
                            format!(" {}", uids.join(" "))
                        };
                        out.extend(write(format!(
                            "* SEARCH{list}\r\n{tag} OK SEARCH completed\r\n"
                        )));
                    }
                    "FETCH" => {
                        let uid: u32 = rest.split(' ').next().unwrap().parse().unwrap();
                        if let Some((seq, email)) =
                            emails.iter().enumerate().find(|(_, e)| e.uid == uid)
                        {
                            out.extend(write(format!(
                                "* {} FETCH (UID {uid} RFC822.SIZE {} BODY[] {{{}}}\r\n",
                                seq + 1,
                                email.data.len(),
                                email.data.len()
                            )));
                            out.extend(&email.data);
                            out.extend(b")\r\n");
                        }
                        out.extend(write(format!("{tag} OK FETCH completed\r\n")));
                    }
                    "STORE" => {
                        let uid: u32 = rest.split(' ').next().unwrap().parse().unwrap();
                        for (seq, email) in emails.iter_mut().enumerate() {
                            if email.uid == uid {
                                email.seen |= rest.contains("\\Seen");
                                email.deleted |= rest.contains("\\Deleted");
                                out.extend(write(format!(
                                    "* {} FETCH (UID {uid} FLAGS (\\Seen))\r\n",
                                    seq + 1
                                )));
                            }
                        }
                        out.extend(write(format!("{tag} OK STORE completed\r\n")));
                    }
                    "MOVE" => {
                        let args = arguments(rest);
                        let uid: u32 = args[0].parse().unwrap();
                        for email in emails.iter_mut() {
                            if email.uid == uid {
                                email.folder = args[1].clone();
                            }
                        }
                        out.extend(write(format!("{tag} OK MOVE completed\r\n")));
                    }
                    _ => out.extend(write(format!("{tag} BAD unknown UID command\r\n"))),
                }
            }
            "EXPUNGE" => {
                server.emails.lock().unwrap().retain(|e| !e.deleted);
                out.extend(write(format!("{tag} OK EXPUNGE completed\r\n")));
            }
            "NOOP" => out.extend(write(format!("{tag} OK NOOP completed\r\n"))),
            "LOGOUT" => {
                out.extend(write(format!(
                    "* BYE logging out\r\n{tag} OK LOGOUT completed\r\n"
                )));
                control.get_mut().write_all(&out).await.unwrap();
                return;
            }
            _ => out.extend(write(format!("{tag} BAD unknown command\r\n"))),
        }
        if control.get_mut().write_all(&out).await.is_err() {
            return;
        }
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    port: u16,
    ca: std::path::PathBuf,
    server: Arc<Server>,
}

async fn start(tls: bool, emails: Vec<Email>) -> Fixture {
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
    let server = Arc::new(Server {
        emails: Mutex::new(emails),
    });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let shared = server.clone();
    tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                return;
            };
            let server = shared.clone();
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                match acceptor {
                    Some(acceptor) => {
                        if let Ok(stream) = acceptor.accept(socket).await {
                            session(stream, server).await;
                        }
                    }
                    None => session(socket, server).await,
                }
            });
        }
    });
    Fixture {
        _dir: dir,
        port,
        ca,
        server,
    }
}

fn email(uid: u32, subject: &str, attachment: &str) -> Email {
    let data = format!(
        "From: Analyzer <analyzer@lab.test>\r\n\
Subject: {subject}\r\n\
Message-ID: <{uid}@lab.test>\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"b\"\r\n\
\r\n\
--b\r\n\
Content-Type: text/plain\r\n\
\r\n\
Synthetic results attached.\r\n\
--b\r\n\
Content-Type: text/plain\r\n\
Content-Disposition: attachment; filename=\"result-{uid}.hl7\"\r\n\
\r\n\
{attachment}\r\n\
--b--\r\n"
    );
    Email {
        uid,
        data: data.into_bytes(),
        seen: false,
        deleted: false,
        folder: "INBOX".into(),
    }
}

const HL7: &str =
    "MSH|^~\\&|ANALYZER|LAB|LIS|HOSP|20260929120000||ORU^R01|I1|P|2.5.1\rPID|1||SYNTH-5";

#[tokio::test(flavor = "multi_thread")]
async fn reads_whole_emails_and_flags_them_seen() {
    let fixture = start(false, vec![email(1, "Run 1", HL7)]).await;
    let recorder = Arc::new(Recorder::default());
    let engine = engine(recorder.clone()).await;
    engine
        .deploy(channel(&format!(
            "  type: imap
  data_type: raw
  settings:
    host: 127.0.0.1
    port: {}
    security: none
    username: lab
    password: {PASSWORD}
    poll_interval: 100ms",
            fixture.port
        )))
        .await
        .unwrap();
    wait_until("the email is stored", || recorder.payloads().len() == 1).await;
    let stored = String::from_utf8(recorder.payloads()[0].clone()).unwrap();
    assert!(stored.contains("Subject: Run 1"), "{stored}");
    wait_until("the email is flagged", || {
        fixture.server.emails.lock().unwrap()[0].seen
    })
    .await;
    // Later checks do not take it again.
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(recorder.payloads().len(), 1);
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn takes_attachments_over_tls_and_moves_emails() {
    let fixture = start(true, vec![email(7, "Run 7", HL7), email(8, "Run 8", HL7)]).await;
    let recorder = Arc::new(Recorder::default());
    let engine = engine(recorder.clone()).await;
    engine
        .deploy(channel(&format!(
            "  type: imap
  data_type: hl7v2
  settings:
    host: 127.0.0.1
    port: {}
    tls: {{ca_file: {}, system_roots: false, server_name: localhost}}
    username: lab
    password: {PASSWORD}
    content: attachments
    attachment_pattern: '*.hl7'
    after: move
    move_to: Processed/Lab
    poll_interval: 100ms",
            fixture.port,
            yaml_path(&fixture.ca)
        )))
        .await
        .unwrap();
    wait_until("both attachments are stored", || {
        recorder.payloads().len() == 2
    })
    .await;
    assert!(recorder.payloads()[0].starts_with(b"MSH|"));
    wait_until("the emails are moved", || {
        fixture
            .server
            .emails
            .lock()
            .unwrap()
            .iter()
            .all(|e| e.folder == "Processed/Lab")
    })
    .await;
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn deletes_stored_emails() {
    let fixture = start(false, vec![email(3, "Run 3", HL7)]).await;
    let recorder = Arc::new(Recorder::default());
    let engine = engine(recorder.clone()).await;
    engine
        .deploy(channel(&format!(
            "  type: imap
  data_type: raw
  settings:
    host: 127.0.0.1
    port: {}
    security: none
    username: lab
    password: {PASSWORD}
    after: delete
    poll_interval: 100ms",
            fixture.port
        )))
        .await
        .unwrap();
    wait_until("the email is stored", || recorder.payloads().len() == 1).await;
    wait_until("the email is expunged", || {
        fixture.server.emails.lock().unwrap().is_empty()
    })
    .await;
    engine.shutdown().await;
}
