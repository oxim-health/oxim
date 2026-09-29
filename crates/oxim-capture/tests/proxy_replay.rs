//! A conversation recorded through the proxy replays against a new host.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use oxim_capture::{
    Capture, CaptureSink, DeviceSide, Direction, Endpoint, Event, Header, Protocol, ReplayOptions,
    TcpProxy, Transport, now, replay, replay_listening,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

/// A host that answers every MLLP frame with `ACK<n>`; returns the frames it
/// received once the connection closes.
async fn mllp_host(listener: TcpListener, connections: usize) -> Vec<Vec<u8>> {
    let mut frames = Vec::new();
    for _ in 0..connections {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 1024];
        loop {
            let n = stream.read(&mut chunk).await.unwrap();
            if n == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..n]);
            while let Some(end) = buffer.windows(2).position(|w| w == [0x1C, 0x0D]) {
                let frame: Vec<u8> = buffer.drain(..end + 2).collect();
                frames.push(frame);
                let answer = format!("\x0bACK{}\x1c\r", frames.len());
                stream.write_all(answer.as_bytes()).await.unwrap();
            }
        }
    }
    frames
}

/// A device that sends frames and waits for each answer.
async fn device(mut stream: TcpStream, frames: &[&[u8]]) {
    for frame in frames {
        stream.write_all(frame).await.unwrap();
        let mut answer = Vec::new();
        let mut chunk = [0u8; 64];
        while !answer.ends_with(&[0x1C, 0x0D]) {
            let n = stream.read(&mut chunk).await.unwrap();
            assert!(n > 0, "the host closed early");
            answer.extend_from_slice(&chunk[..n]);
        }
    }
}

const FRAMES: [&[u8]; 2] = [b"\x0bMSH|one\x1c\r", b"\x0bMSH|two\x1c\r"];

#[tokio::test(flavor = "multi_thread")]
async fn records_and_replays_a_conversation() {
    let host = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let host_address = host.local_addr().unwrap().to_string();
    let host_task = tokio::spawn(mllp_host(host, 1));

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.oximcap");
    let mut header = Header::new(Transport::Tcp, now());
    header.protocol = Some(Protocol::Hl7v2Mllp);
    let sink = CaptureSink::new(std::fs::File::create(&path).unwrap(), &header).unwrap();
    let proxy = TcpProxy::bind("127.0.0.1:0", host_address, DeviceSide::Client)
        .await
        .unwrap();
    let proxy_address = proxy.local_addr().unwrap();
    let (stop, stopped) = oneshot::channel::<()>();
    let proxy_task = tokio::spawn(proxy.run(sink.clone(), async {
        let _ = stopped.await;
    }));

    device(TcpStream::connect(proxy_address).await.unwrap(), &FRAMES).await;
    let received = host_task.await.unwrap();
    assert_eq!(
        received,
        FRAMES.iter().map(|f| f.to_vec()).collect::<Vec<_>>()
    );
    // Wait for the close event, then stop the proxy.
    for _ in 0..100 {
        if Capture::load(&path)
            .unwrap()
            .records
            .iter()
            .any(|r| r.event == Event::Close)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let _ = stop.send(());
    proxy_task.await.unwrap().unwrap();

    let capture = Capture::load(&path).unwrap();
    assert_eq!(capture.header.protocol, Some(Protocol::Hl7v2Mllp));
    assert_eq!(capture.stream(1, Direction::DeviceToHost), FRAMES.concat());
    assert_eq!(
        capture.stream(1, Direction::HostToDevice),
        b"\x0bACK1\x1c\r\x0bACK2\x1c\r"
    );
    assert_eq!(capture.records.first().unwrap().event, Event::Open);

    // Replay against a new host.
    let host = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = Endpoint::Connect(host.local_addr().unwrap().to_string());
    let host_task = tokio::spawn(mllp_host(host, 1));
    let report = replay(&capture, &endpoint, &ReplayOptions::default())
        .await
        .unwrap();
    assert_eq!(report.connections, 1);
    assert_eq!(report.answers, 2);
    assert_eq!(report.unanswered, 0);
    assert_eq!(report.host_bytes(), b"\x0bACK1\x1c\r\x0bACK2\x1c\r");
    assert_eq!(host_task.await.unwrap().len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn replays_as_a_listening_device_and_notes_missing_answers() {
    let mut capture = Capture::new(Header::new(Transport::Tcp, now()));
    capture.records = vec![
        oxim_capture::Record::data(
            now(),
            1,
            Direction::DeviceToHost,
            Transport::Tcp,
            b"ping".to_vec(),
        ),
        oxim_capture::Record::data(
            now(),
            1,
            Direction::HostToDevice,
            Transport::Tcp,
            b"pong".to_vec(),
        ),
    ];
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    // The host connects, reads the ping and never answers.
    let host = tokio::spawn(async move {
        let mut stream = TcpStream::connect(address).await.unwrap();
        let mut ping = [0u8; 4];
        stream.read_exact(&mut ping).await.unwrap();
        tokio::time::sleep(Duration::from_millis(400)).await;
        ping
    });
    let options = ReplayOptions {
        answer_timeout: Duration::from_millis(200),
        ..ReplayOptions::default()
    };
    let report = replay_listening(&capture, listener, &options)
        .await
        .unwrap();
    assert_eq!(&host.await.unwrap(), b"ping");
    assert_eq!(report.answers, 0);
    assert_eq!(report.unanswered, 1);
}
