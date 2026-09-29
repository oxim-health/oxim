//! FTP and FTPS source and destination against a small in-process FTP
//! server that serves a temporary directory.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::{Recorder, channel, delivery, destination, engine, wait_until, yaml_path};
use oxim_model::DataType;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;

const PASSWORD: &str = "synthetic-ftp-password";

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

struct State {
    root: PathBuf,
    tls: Option<TlsAcceptor>,
}

impl State {
    fn resolve(&self, path: &str) -> PathBuf {
        let mut resolved = self.root.clone();
        for part in path.split('/') {
            if !matches!(part, "" | "." | "..") {
                resolved.push(part);
            }
        }
        resolved
    }
}

async fn reply<S: AsyncWrite + Unpin>(control: &mut S, line: &str) {
    control
        .write_all(format!("{line}\r\n").as_bytes())
        .await
        .unwrap();
    control.flush().await.unwrap();
}

async fn read_command<S: AsyncRead + Unpin>(
    control: &mut BufReader<S>,
) -> Option<(String, String)> {
    let mut line = String::new();
    if control.read_line(&mut line).await.ok()? == 0 {
        return None;
    }
    let line = line.trim_end();
    let (command, argument) = line.split_once(' ').unwrap_or((line, ""));
    Some((command.to_ascii_uppercase(), argument.to_owned()))
}

async fn session(socket: TcpStream, state: Arc<State>) {
    let mut control = BufReader::new(socket);
    reply(control.get_mut(), "220 synthetic FTP server").await;
    let Some((command, argument)) = read_command(&mut control).await else {
        return;
    };
    if command == "AUTH" && argument.eq_ignore_ascii_case("TLS") {
        let Some(acceptor) = state.tls.clone() else {
            reply(control.get_mut(), "502 TLS not configured").await;
            return;
        };
        reply(control.get_mut(), "234 Proceed with negotiation").await;
        let Ok(tls) = acceptor.accept(control.into_inner()).await else {
            return;
        };
        commands(BufReader::new(tls), None, &state).await;
    } else {
        commands(control, Some((command, argument)), &state).await;
    }
}

async fn data_connection(
    passive: &mut Option<TcpListener>,
    protect: bool,
    state: &State,
) -> Box<dyn Io> {
    let listener = passive.take().expect("PASV before the transfer");
    let (socket, _) = listener.accept().await.unwrap();
    match (&state.tls, protect) {
        (Some(acceptor), true) => Box::new(acceptor.accept(socket).await.unwrap()),
        _ => Box::new(socket),
    }
}

async fn commands<S: AsyncRead + AsyncWrite + Unpin>(
    mut control: BufReader<S>,
    mut pending: Option<(String, String)>,
    state: &State,
) {
    let mut passive: Option<TcpListener> = None;
    let mut protect = false;
    let mut rename_from: Option<PathBuf> = None;
    loop {
        let (command, argument) = match pending.take() {
            Some(first) => first,
            None => match read_command(&mut control).await {
                Some(next) => next,
                None => return,
            },
        };
        let out = control.get_mut();
        match command.as_str() {
            "USER" => reply(out, "331 Password required").await,
            "PASS" if argument == PASSWORD => reply(out, "230 Logged in").await,
            "PASS" => reply(out, "530 Login incorrect").await,
            "TYPE" | "OPTS" => reply(out, "200 OK").await,
            "PBSZ" => reply(out, "200 PBSZ=0").await,
            "PROT" => {
                protect = argument.eq_ignore_ascii_case("P");
                reply(out, "200 Protection level set").await;
            }
            "SYST" => reply(out, "215 UNIX Type: L8").await,
            "PWD" => reply(out, "257 \"/\" is the current directory").await,
            "PASV" => {
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let port = listener.local_addr().unwrap().port();
                passive = Some(listener);
                reply(
                    out,
                    &format!(
                        "227 Entering Passive Mode (127,0,0,1,{},{})",
                        port / 256,
                        port % 256
                    ),
                )
                .await;
            }
            "LIST" => {
                let directory = state.resolve(&argument);
                reply(out, "150 Here comes the directory listing").await;
                let mut data = data_connection(&mut passive, protect, state).await;
                let mut listing = String::new();
                if let Ok(entries) = std::fs::read_dir(&directory) {
                    for entry in entries {
                        let entry = entry.unwrap();
                        let metadata = entry.metadata().unwrap();
                        let kind = if metadata.is_dir() { 'd' } else { '-' };
                        listing.push_str(&format!(
                            "{kind}rw-r--r--    1 1000     1000     {:>8} Sep 29 12:00 {}\r\n",
                            metadata.len(),
                            entry.file_name().to_string_lossy()
                        ));
                    }
                }
                data.write_all(listing.as_bytes()).await.unwrap();
                data.shutdown().await.ok();
                drop(data);
                reply(control.get_mut(), "226 Directory send OK").await;
            }
            "RETR" => {
                let path = state.resolve(&argument);
                match std::fs::read(&path) {
                    Ok(content) => {
                        reply(out, "150 Opening data connection").await;
                        let mut data = data_connection(&mut passive, protect, state).await;
                        data.write_all(&content).await.unwrap();
                        data.shutdown().await.ok();
                        drop(data);
                        reply(control.get_mut(), "226 Transfer complete").await;
                    }
                    Err(_) => reply(out, "550 No such file").await,
                }
            }
            "STOR" => {
                let path = state.resolve(&argument);
                reply(out, "150 Ok to send data").await;
                let mut data = data_connection(&mut passive, protect, state).await;
                let mut content = Vec::new();
                // A TLS peer may close without close_notify after the data.
                let _ = data.read_to_end(&mut content).await;
                drop(data);
                std::fs::write(&path, content).unwrap();
                reply(control.get_mut(), "226 Transfer complete").await;
            }
            "SIZE" => match std::fs::metadata(state.resolve(&argument)) {
                Ok(metadata) if metadata.is_file() => {
                    reply(out, &format!("213 {}", metadata.len())).await;
                }
                _ => reply(out, "550 Could not get file size").await,
            },
            "DELE" => match std::fs::remove_file(state.resolve(&argument)) {
                Ok(()) => reply(out, "250 Deleted").await,
                Err(_) => reply(out, "550 Delete failed").await,
            },
            "RNFR" => {
                let path = state.resolve(&argument);
                if path.exists() {
                    rename_from = Some(path);
                    reply(out, "350 Ready for RNTO").await;
                } else {
                    reply(out, "550 No such file").await;
                }
            }
            "RNTO" => {
                let target = state.resolve(&argument);
                match rename_from.take() {
                    Some(from) if !target.exists() => {
                        std::fs::rename(from, target).unwrap();
                        reply(out, "250 Rename successful").await;
                    }
                    _ => reply(out, "553 Rename failed").await,
                }
            }
            "MKD" => match std::fs::create_dir(state.resolve(&argument)) {
                Ok(()) => reply(out, &format!("257 \"{argument}\" created")).await,
                Err(_) => reply(out, "550 Create directory operation failed").await,
            },
            "QUIT" => {
                reply(out, "221 Goodbye").await;
                return;
            }
            // MLSD is left out on purpose, so the LIST fallback is used.
            _ => reply(out, "500 Unknown command").await,
        }
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    port: u16,
    ca: PathBuf,
}

async fn start(tls: bool) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(root.join("in")).unwrap();
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
    let state = Arc::new(State {
        root: root.clone(),
        tls: acceptor,
    });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(session(socket, state.clone()));
        }
    });
    Fixture {
        _dir: dir,
        root,
        port,
        ca,
    }
}

fn files(directory: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(directory)
        .map(|entries| {
            entries
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

const ORDER: &[u8] =
    b"MSH|^~\\&|LIS|LAB|OXIM|LAB|20260929100000||OML^O21|F1|P|2.5.1\rPID|1||SYNTH-9\r";

#[tokio::test(flavor = "multi_thread")]
async fn polls_over_plain_ftp_and_deletes_stored_files() {
    let fixture = start(false).await;
    let recorder = Arc::new(Recorder::default());
    let engine = engine(recorder.clone()).await;
    std::fs::write(fixture.root.join("in/order-1.hl7"), ORDER).unwrap();
    engine
        .deploy(channel(&format!(
            "  type: ftp
  data_type: hl7v2
  settings:
    host: 127.0.0.1
    port: {}
    security: none
    username: lab
    password: {PASSWORD}
    directory: in
    poll_interval: 100ms
    after: delete",
            fixture.port
        )))
        .await
        .unwrap();
    wait_until("the file is stored", || recorder.payloads().len() == 1).await;
    assert_eq!(recorder.payloads()[0], ORDER);
    wait_until("the file is deleted", || {
        files(&fixture.root.join("in")).is_empty()
    })
    .await;
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn writes_over_explicit_ftps() {
    let fixture = start(true).await;
    let sender = destination(
        "ftp",
        &format!(
            "      host: 127.0.0.1
      port: {}
      username: lab
      password: {PASSWORD}
      tls: {{ca_file: {}, system_roots: false, server_name: localhost}}
      directory: out/lis
      filename: 'order-{{message_id}}.{{extension}}'",
            fixture.port,
            yaml_path(&fixture.ca)
        ),
    );
    sender
        .send(&delivery(1, ORDER, DataType::Hl7V2))
        .await
        .unwrap();
    sender
        .send(&delivery(1, ORDER, DataType::Hl7V2))
        .await
        .unwrap();
    let written = files(&fixture.root.join("out/lis"));
    assert_eq!(written.len(), 1, "{written:?}");
    assert!(written[0].starts_with("order-") && written[0].ends_with(".hl7"));
    assert_eq!(
        std::fs::read(fixture.root.join("out/lis").join(&written[0])).unwrap(),
        ORDER
    );

    // Plain FTP against a server that insists on TLS is not attempted
    // silently: the untrusted certificate fails the handshake.
    let untrusted = destination(
        "ftp",
        &format!(
            "      host: 127.0.0.1
      port: {}
      username: lab
      password: {PASSWORD}
      tls: {{system_roots: false, ca_file: {}, server_name: other.example}}",
            fixture.port,
            yaml_path(&fixture.ca)
        ),
    );
    let error = untrusted
        .send(&delivery(2, ORDER, DataType::Hl7V2))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("TLS"), "{error}");
}

#[tokio::test(flavor = "multi_thread")]
async fn reports_bad_credentials() {
    let fixture = start(false).await;
    let sender = destination(
        "ftp",
        &format!(
            "      host: 127.0.0.1
      port: {}
      security: none
      username: lab
      password: wrong",
            fixture.port
        ),
    );
    let error = sender
        .send(&delivery(1, ORDER, DataType::Hl7V2))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("login as lab failed"), "{error}");
}
