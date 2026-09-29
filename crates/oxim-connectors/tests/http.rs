//! HTTP destination against a minimal local server.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{delivery, destination};
use oxim_model::DataType;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// A received request: head (request line and headers) and body.
#[derive(Debug)]
struct Received {
    head: String,
    body: Vec<u8>,
}

/// Answers each connection with the next `(status line, body)`; `None`
/// accepts the connection and never answers.
async fn server(
    listener: TcpListener,
    replies: Vec<Option<(&'static str, &'static str)>>,
) -> Vec<Received> {
    let mut received = Vec::new();
    let mut silent = Vec::new();
    for reply in replies {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut data = Vec::new();
        let mut buffer = [0u8; 4096];
        let head_end = loop {
            let n = stream.read(&mut buffer).await.unwrap();
            data.extend_from_slice(&buffer[..n]);
            if let Some(at) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                break at + 4;
            }
        };
        let head = String::from_utf8_lossy(&data[..head_end]).into_owned();
        let length: usize = head
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(|value| value.trim().parse().unwrap())
            })
            .unwrap_or(0);
        while data.len() < head_end + length {
            let n = stream.read(&mut buffer).await.unwrap();
            data.extend_from_slice(&buffer[..n]);
        }
        received.push(Received {
            head,
            body: data[head_end..head_end + length].to_vec(),
        });
        match reply {
            Some((status, body)) => {
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            }
            None => silent.push(stream),
        }
    }
    received
}

#[tokio::test(flavor = "multi_thread")]
async fn posts_messages_and_classifies_responses() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(server(
        listener,
        vec![
            Some(("200 OK", "stored")),
            Some(("503 Service Unavailable", "busy")),
            Some(("400 Bad Request", "invalid")),
            None,
        ],
    ));
    let http = destination(
        "http",
        &format!(
            "      url: 'http://127.0.0.1:{port}/api/results'\n      headers: {{Authorization: 'Bearer secret'}}\n      timeout: 500ms"
        ),
    );
    let ok = delivery(1, br#"{"code":"GLU"}"#, DataType::Json);
    assert_eq!(http.send(&ok).await.unwrap(), Some(b"stored".to_vec()));
    let busy = http
        .send(&delivery(2, b"x", DataType::Json))
        .await
        .unwrap_err();
    assert!(!busy.permanent && busy.message.contains("503"), "{busy:?}");
    let invalid = http
        .send(&delivery(3, b"x", DataType::Json))
        .await
        .unwrap_err();
    assert!(
        invalid.permanent && invalid.message.contains("invalid"),
        "{invalid:?}"
    );
    let timeout = http
        .send(&delivery(4, b"x", DataType::Json))
        .await
        .unwrap_err();
    assert!(!timeout.permanent, "{timeout:?}");

    let received = server.await.unwrap();
    let first = &received[0];
    assert!(
        first.head.starts_with("POST /api/results HTTP/1.1\r\n"),
        "{}",
        first.head
    );
    let head = first.head.to_ascii_lowercase();
    assert!(head.contains("content-type: application/json\r\n"));
    assert!(head.contains("authorization: bearer secret\r\n"));
    assert!(head.contains(&format!(
        "x-oxim-message-id: {}\r\n",
        ok.message_id.to_string().to_ascii_lowercase()
    )));
    assert_eq!(first.body, br#"{"code":"GLU"}"#);
}

#[tokio::test(flavor = "multi_thread")]
async fn authenticates_with_oauth2_client_credentials() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(server(
        listener,
        vec![
            Some((
                "200 OK",
                r#"{"access_token":"token-a","token_type":"Bearer","expires_in":3600}"#,
            )),
            Some(("200 OK", "stored")),
            Some(("200 OK", "stored")),
            Some(("401 Unauthorized", "")),
            Some((
                "200 OK",
                r#"{"access_token":"token-b","token_type":"bearer"}"#,
            )),
            Some(("200 OK", "stored")),
        ],
    ));
    let http = destination(
        "http",
        &format!(
            "      url: 'http://127.0.0.1:{port}/api/results'
      oauth2:
        token_url: 'http://127.0.0.1:{port}/token'
        client_id: lab client
        client_secret: synthetic/secret
        scope: results.write"
        ),
    );
    for n in 1..=2 {
        http.send(&delivery(n, b"{}", DataType::Json))
            .await
            .unwrap();
    }
    let rejected = http
        .send(&delivery(3, b"{}", DataType::Json))
        .await
        .unwrap_err();
    assert!(
        !rejected.permanent && rejected.message.contains("OAuth"),
        "{rejected:?}"
    );
    http.send(&delivery(3, b"{}", DataType::Json))
        .await
        .unwrap();

    let received = server.await.unwrap();
    let heads: Vec<String> = received
        .iter()
        .map(|r| r.head.to_ascii_lowercase())
        .collect();
    assert!(heads[0].starts_with("post /token "), "{}", heads[0]);
    // Basic base64("lab+client:synthetic%2Fsecret"): form-encoded first.
    assert!(
        heads[0].contains("authorization: basic bgfik2nsawvuddpzew50agv0awmlmkzzzwnyzxq=\r\n"),
        "{}",
        heads[0]
    );
    assert_eq!(
        received[0].body,
        b"grant_type=client_credentials&scope=results.write"
    );
    assert!(heads[1].contains("authorization: bearer token-a\r\n"));
    assert!(
        heads[2].contains("authorization: bearer token-a\r\n"),
        "the token is cached"
    );
    assert!(
        heads[4].starts_with("post /token "),
        "a new token after the 401"
    );
    assert!(heads[5].contains("authorization: bearer token-b\r\n"));
}

#[tokio::test(flavor = "multi_thread")]
async fn connection_errors_are_temporary() {
    let port = common::free_port();
    let http = destination(
        "http",
        &format!("      url: 'http://127.0.0.1:{port}/'\n      method: put\n      timeout: 2s"),
    );
    let error = http
        .send(&delivery(1, b"x", DataType::Hl7V2))
        .await
        .unwrap_err();
    assert!(!error.permanent, "{error:?}");
}
