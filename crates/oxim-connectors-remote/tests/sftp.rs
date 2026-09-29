//! SFTP source and destination against an in-process SSH server whose SFTP
//! subsystem serves a temporary directory.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::{Recorder, channel, delivery, destination, engine, wait_until, yaml_path};
use oxim_model::DataType;
use russh::keys::ssh_key::LineEnding;
use russh::keys::{Algorithm, HashAlg, PrivateKey};
use russh::server::{Auth, Msg, Server as _, Session};
use russh::{Channel, ChannelId};
use russh_sftp::protocol::{
    Attrs, Data, File, FileAttributes, Handle, Name, OpenFlags, Status, StatusCode,
};
use tokio::net::TcpListener;

const PASSWORD: &str = "synthetic-test-password";

/// SFTP over a directory.
struct Files {
    root: PathBuf,
    next: u64,
    handles: HashMap<String, PathBuf>,
    listings: HashMap<String, Option<Vec<File>>>,
}

impl Files {
    fn resolve(&self, path: &str) -> Result<PathBuf, StatusCode> {
        let mut resolved = self.root.clone();
        for part in path.split('/') {
            match part {
                "" | "." => {}
                ".." => return Err(StatusCode::PermissionDenied),
                other => resolved.push(other),
            }
        }
        Ok(resolved)
    }

    fn handle(&mut self, prefix: &str) -> String {
        self.next += 1;
        format!("{prefix}{}", self.next)
    }

    fn ok(id: u32) -> Status {
        Status {
            id,
            status_code: StatusCode::Ok,
            error_message: "Ok".into(),
            language_tag: "en-US".into(),
        }
    }
}

fn io_status(e: &std::io::Error) -> StatusCode {
    match e.kind() {
        std::io::ErrorKind::NotFound => StatusCode::NoSuchFile,
        std::io::ErrorKind::PermissionDenied => StatusCode::PermissionDenied,
        _ => StatusCode::Failure,
    }
}

impl russh_sftp::server::Handler for Files {
    type Error = StatusCode;

    fn unimplemented(&self) -> Self::Error {
        StatusCode::OpUnsupported
    }

    async fn open(
        &mut self,
        id: u32,
        filename: String,
        pflags: OpenFlags,
        _attrs: FileAttributes,
    ) -> Result<Handle, Self::Error> {
        let path = self.resolve(&filename)?;
        if pflags.contains(OpenFlags::CREATE) {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create(true);
            if pflags.contains(OpenFlags::TRUNCATE) {
                options.truncate(true);
            }
            options.open(&path).map_err(|e| io_status(&e))?;
        } else if !path.is_file() {
            return Err(StatusCode::NoSuchFile);
        }
        let handle = self.handle("f");
        self.handles.insert(handle.clone(), path);
        Ok(Handle { id, handle })
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, Self::Error> {
        self.handles.remove(&handle);
        self.listings.remove(&handle);
        Ok(Self::ok(id))
    }

    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> Result<Data, Self::Error> {
        let path = self.handles.get(&handle).ok_or(StatusCode::Failure)?;
        let mut file = std::fs::File::open(path).map_err(|e| io_status(&e))?;
        let size = file.metadata().map_err(|e| io_status(&e))?.len();
        if offset >= size {
            return Err(StatusCode::Eof);
        }
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| io_status(&e))?;
        let mut data = vec![0; usize::try_from(len.min(32 * 1024)).unwrap()];
        let n = file.read(&mut data).map_err(|e| io_status(&e))?;
        data.truncate(n);
        Ok(Data { id, data })
    }

    async fn write(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<Status, Self::Error> {
        let path = self.handles.get(&handle).ok_or(StatusCode::Failure)?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .map_err(|e| io_status(&e))?;
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| io_status(&e))?;
        file.write_all(&data).map_err(|e| io_status(&e))?;
        Ok(Self::ok(id))
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        let metadata = std::fs::metadata(self.resolve(&path)?).map_err(|e| io_status(&e))?;
        Ok(Attrs {
            id,
            attrs: FileAttributes::from(&metadata),
        })
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        self.stat(id, path).await
    }

    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, Self::Error> {
        let directory = self.resolve(&path)?;
        let mut files = Vec::new();
        for entry in std::fs::read_dir(&directory).map_err(|e| io_status(&e))? {
            let entry = entry.map_err(|e| io_status(&e))?;
            let metadata = entry.metadata().map_err(|e| io_status(&e))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            files.push(File::new(name, FileAttributes::from(&metadata)));
        }
        let handle = self.handle("d");
        self.listings.insert(handle.clone(), Some(files));
        Ok(Handle { id, handle })
    }

    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, Self::Error> {
        match self.listings.get_mut(&handle).and_then(Option::take) {
            Some(files) if !files.is_empty() => Ok(Name { id, files }),
            _ => Err(StatusCode::Eof),
        }
    }

    async fn remove(&mut self, id: u32, filename: String) -> Result<Status, Self::Error> {
        std::fs::remove_file(self.resolve(&filename)?).map_err(|e| io_status(&e))?;
        Ok(Self::ok(id))
    }

    async fn mkdir(
        &mut self,
        id: u32,
        path: String,
        _attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        std::fs::create_dir(self.resolve(&path)?).map_err(|e| io_status(&e))?;
        Ok(Self::ok(id))
    }

    async fn rename(
        &mut self,
        id: u32,
        oldpath: String,
        newpath: String,
    ) -> Result<Status, Self::Error> {
        let target = self.resolve(&newpath)?;
        if target.exists() {
            return Err(StatusCode::Failure);
        }
        std::fs::rename(self.resolve(&oldpath)?, target).map_err(|e| io_status(&e))?;
        Ok(Self::ok(id))
    }

    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        Ok(Name {
            id,
            files: vec![File::dummy(format!(
                "/{}",
                path.trim_start_matches(['.', '/'])
            ))],
        })
    }
}

#[derive(Clone)]
struct Server {
    root: PathBuf,
    client_key: russh::keys::ssh_key::PublicKey,
}

struct Connection {
    root: PathBuf,
    client_key: russh::keys::ssh_key::PublicKey,
    channels: HashMap<ChannelId, Channel<Msg>>,
}

impl russh::server::Server for Server {
    type Handler = Connection;

    fn new_client(&mut self, _: Option<std::net::SocketAddr>) -> Connection {
        Connection {
            root: self.root.clone(),
            client_key: self.client_key.clone(),
            channels: HashMap::new(),
        }
    }
}

impl russh::server::Handler for Connection {
    type Error = russh::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        Ok(if user == "lab" && password == PASSWORD {
            Auth::Accept
        } else {
            Auth::reject()
        })
    }

    async fn auth_publickey(
        &mut self,
        user: &str,
        key: &russh::keys::ssh_key::PublicKey,
    ) -> Result<Auth, Self::Error> {
        Ok(
            if user == "lab" && key.key_data() == self.client_key.key_data() {
                Auth::Accept
            } else {
                Auth::reject()
            },
        )
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: russh::server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.channels.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        channel_id: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        match (name, self.channels.remove(&channel_id)) {
            ("sftp", Some(channel)) => {
                session.channel_success(channel_id)?;
                let files = Files {
                    root: self.root.clone(),
                    next: 0,
                    handles: HashMap::new(),
                    listings: HashMap::new(),
                };
                russh_sftp::server::run(channel.into_stream(), files).await;
            }
            _ => session.channel_failure(channel_id)?,
        }
        Ok(())
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    port: u16,
    fingerprint: String,
    known_hosts: PathBuf,
    client_key: PathBuf,
}

async fn start() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir_all(root.join("in")).unwrap();
    let host_key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
    let client = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
    let client_key = dir.path().join("id_ed25519");
    std::fs::write(
        &client_key,
        client.to_openssh(LineEnding::LF).unwrap().as_bytes(),
    )
    .unwrap();
    let fingerprint = host_key
        .public_key()
        .fingerprint(HashAlg::Sha256)
        .to_string();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let known_hosts = dir.path().join("known_hosts");
    std::fs::write(
        &known_hosts,
        format!(
            "[127.0.0.1]:{port} {}\n",
            host_key.public_key().to_openssh().unwrap()
        ),
    )
    .unwrap();
    let config = Arc::new(russh::server::Config {
        keys: vec![host_key],
        auth_rejection_time: std::time::Duration::from_millis(10),
        auth_rejection_time_initial: Some(std::time::Duration::from_millis(0)),
        ..russh::server::Config::default()
    });
    let mut server = Server {
        root: root.clone(),
        client_key: client.public_key().clone(),
    };
    tokio::spawn(async move {
        let _ = server.run_on_socket(config, &listener).await;
    });
    Fixture {
        _dir: dir,
        root,
        port,
        fingerprint,
        known_hosts,
        client_key,
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

const RESULT: &[u8] =
    b"MSH|^~\\&|ANALYZER|LAB|LIS|HOSP|20260929120000||ORU^R01|S1|P|2.5.1\rPID|1||SYNTH-1\r";
const SECOND: &[u8] =
    b"MSH|^~\\&|ANALYZER|LAB|LIS|HOSP|20260929120500||ORU^R01|S2|P|2.5.1\rPID|1||SYNTH-2\r";

#[tokio::test(flavor = "multi_thread")]
async fn polls_a_remote_directory_and_moves_stored_files() {
    let fixture = start().await;
    let recorder = Arc::new(Recorder::default());
    let engine = engine(recorder.clone()).await;
    std::fs::write(fixture.root.join("in/result-1.hl7"), RESULT).unwrap();
    std::fs::write(fixture.root.join("in/ignored.tmp"), b"not matching").unwrap();
    engine
        .deploy(channel(&format!(
            "  type: sftp
  data_type: hl7v2
  settings:
    host: 127.0.0.1
    port: {}
    username: lab
    password: {PASSWORD}
    known_hosts_file: {}
    directory: in
    pattern: '*.hl7'
    poll_interval: 100ms
    processed_directory: done/today",
            fixture.port,
            yaml_path(&fixture.known_hosts)
        )))
        .await
        .unwrap();
    wait_until("the file is stored", || recorder.payloads().len() == 1).await;
    assert_eq!(recorder.payloads()[0], RESULT);
    wait_until("the file is moved", || {
        files(&fixture.root.join("done/today")) == ["result-1.hl7"]
    })
    .await;
    assert_eq!(files(&fixture.root.join("in")), ["ignored.tmp"]);

    // A second file with the same name is moved next to the first.
    std::fs::write(fixture.root.join("in/result-1.hl7"), SECOND).unwrap();
    wait_until("the second file is moved", || {
        files(&fixture.root.join("done/today")) == ["result-1-1.hl7", "result-1.hl7"]
    })
    .await;
    wait_until("the second file is delivered", || {
        recorder.payloads().len() == 2
    })
    .await;
    assert_eq!(recorder.payloads()[1], SECOND);
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn writes_files_atomically_with_key_authentication() {
    let fixture = start().await;
    let sender = destination(
        "sftp",
        &format!(
            "      host: 127.0.0.1
      port: {}
      username: lab
      private_key_file: {}
      host_key_fingerprint: '{}'
      directory: out/lis
      filename: '{{message_id}}.{{extension}}'",
            fixture.port,
            yaml_path(&fixture.client_key),
            fixture.fingerprint
        ),
    );
    sender
        .send(&delivery(1, RESULT, DataType::Hl7V2))
        .await
        .unwrap();
    // The retry of a delivered message succeeds without a second file.
    sender
        .send(&delivery(1, RESULT, DataType::Hl7V2))
        .await
        .unwrap();
    let written = files(&fixture.root.join("out/lis"));
    assert_eq!(written.len(), 1, "{written:?}");
    assert!(written[0].ends_with(".hl7"));
    assert_eq!(
        std::fs::read(fixture.root.join("out/lis").join(&written[0])).unwrap(),
        RESULT
    );
    let error = sender
        .send(&delivery(1, b"different", DataType::Hl7V2))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("different content"), "{error}");
}

#[tokio::test(flavor = "multi_thread")]
async fn refuses_unknown_host_keys_and_bad_credentials() {
    let fixture = start().await;
    let wrong_key = destination(
        "sftp",
        &format!(
            "      host: 127.0.0.1
      port: {}
      username: lab
      password: {PASSWORD}
      host_key_fingerprint: 'SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA'",
            fixture.port
        ),
    );
    let error = wrong_key
        .send(&delivery(1, RESULT, DataType::Hl7V2))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("does not match"), "{error}");

    let empty_known_hosts = fixture.root.join("empty_known_hosts");
    std::fs::write(&empty_known_hosts, "").unwrap();
    let unknown = destination(
        "sftp",
        &format!(
            "      host: 127.0.0.1
      port: {}
      username: lab
      password: {PASSWORD}
      known_hosts_file: {}",
            fixture.port,
            yaml_path(&empty_known_hosts)
        ),
    );
    let error = unknown
        .send(&delivery(1, RESULT, DataType::Hl7V2))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("not listed"), "{error}");

    let bad_password = destination(
        "sftp",
        &format!(
            "      host: 127.0.0.1
      port: {}
      username: lab
      password: wrong
      host_key_fingerprint: '{}'",
            fixture.port, fixture.fingerprint
        ),
    );
    let error = bad_password
        .send(&delivery(1, RESULT, DataType::Hl7V2))
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("rejected the credentials"),
        "{error}"
    );
}

#[test]
fn settings_require_host_key_verification() {
    let config = oxim_core::ChannelConfig::from_yaml(
        "id: x\nsource: {type: sftp, data_type: raw, settings: {host: h, username: u, password: p, directory: in, after: delete}}\n",
    )
    .unwrap();
    let registry = common::registry(Arc::new(Recorder::default()));
    let error = registry.source(&config.source).unwrap_err().to_string();
    assert!(
        error.contains("host key must always be verified"),
        "{error}"
    );
}
