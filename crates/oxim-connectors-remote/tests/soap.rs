//! SOAP destination and source: against each other, and against in-process
//! services that check WS-Security, return faults and require OAuth 2.0.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::convert::Infallible;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use common::{Recorder, channel, delivery, destination, engine, free_port, wait_until};
use http::{Request, Response, StatusCode};
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use oxim_core::ChannelConfig;
use oxim_model::DataType;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

type Handler = Arc<dyn Fn(Request<Bytes>) -> Response<Full<Bytes>> + Send + Sync>;

/// A one-function HTTP server.
async fn serve(handler: Handler) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                return;
            };
            let handler = handler.clone();
            tokio::spawn(async move {
                let service = service_fn(move |request: Request<Incoming>| {
                    let handler = handler.clone();
                    async move {
                        let (parts, body) = request.into_parts();
                        let body = body.collect().await.unwrap().to_bytes();
                        Ok::<_, Infallible>(handler(Request::from_parts(parts, body)))
                    }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(socket), service)
                    .await;
            });
        }
    });
    port
}

fn xml(status: StatusCode, body: &str) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::from(body.to_owned())));
    *response.status_mut() = status;
    response
}

const HL7: &[u8] =
    b"MSH|^~\\&|LAB|HOSP|HIS|HOSP|20260929120000||ORU^R01|W1|P|2.5.1\rPID|1||SYNTH-6\r";

#[tokio::test(flavor = "multi_thread")]
async fn delivers_to_the_soap_endpoint_of_another_channel() {
    let recorder = Arc::new(Recorder::default());
    let engine = engine(recorder.clone()).await;
    let port = free_port();
    engine
        .deploy(channel(&format!(
            "  type: soap
  data_type: xml
  settings: {{listen: '127.0.0.1:{port}', path: /services/results}}"
        )))
        .await
        .unwrap();
    for version in ["1.1", "1.2"] {
        let sender = destination(
            "soap",
            &format!(
                "      url: 'http://127.0.0.1:{port}/services/results'
      version: '{version}'
      action: urn:lab:SubmitResult
      payload_element: 'hl7:Message'
      payload_namespace: 'urn:synthetic:hl7'"
            ),
        );
        let mut answer = None;
        for _ in 0..100 {
            match sender.send(&delivery(1, HL7, DataType::Hl7V2)).await {
                Ok(body) => {
                    answer = body;
                    break;
                }
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(50)).await,
            }
        }
        let answer = String::from_utf8(answer.expect("no answer")).unwrap();
        assert!(answer.starts_with("<oxim:Acknowledgment"), "{answer}");
        assert!(answer.contains("<oxim:MessageId>"), "{answer}");
    }
    wait_until("both messages are stored", || {
        recorder.payloads().len() == 2
    })
    .await;
    let stored = String::from_utf8(recorder.payloads()[0].clone()).unwrap();
    assert!(
        stored.starts_with("<hl7:Message xmlns:hl7=\"urn:synthetic:hl7\">MSH|^~\\&amp;|LAB|"),
        "{stored}"
    );
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn replies_with_the_channel_reply_and_rejects_non_soap() {
    let recorder = Arc::new(Recorder::default());
    let engine = engine(recorder.clone()).await;
    let port = free_port();
    let config = ChannelConfig::from_yaml(&format!(
        "id: echo
source:
  type: soap
  data_type: xml
  response: {{mode: pipeline, encoder: {{type: passthrough}}}}
  settings: {{listen: '127.0.0.1:{port}'}}
"
    ))
    .unwrap();
    engine.deploy(config).await.unwrap();
    let sender = destination(
        "soap",
        &format!("      url: 'http://127.0.0.1:{port}/'\n      version: '1.2'"),
    );
    let mut answer = None;
    for _ in 0..100 {
        match sender
            .send(&delivery(
                2,
                b"<q:Query xmlns:q=\"urn:q\">S123</q:Query>",
                DataType::Xml,
            ))
            .await
        {
            Ok(body) => {
                answer = body;
                break;
            }
            Err(_) => tokio::time::sleep(std::time::Duration::from_millis(50)).await,
        }
    }
    assert_eq!(
        String::from_utf8(answer.unwrap()).unwrap(),
        "<q:Query xmlns:q=\"urn:q\">S123</q:Query>"
    );

    // A request that is not a SOAP envelope gets a Client fault.
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let body = "<notSoap/>";
    stream
        .write_all(
            format!(
                "POST / HTTP/1.1\r\nHost: localhost\r\nContent-Type: text/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    assert!(response.starts_with("HTTP/1.1 500"), "{response}");
    assert!(
        response.contains("<faultcode>soap:Client</faultcode>"),
        "{response}"
    );
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sends_ws_security_and_classifies_faults() {
    let seen: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
    let log = seen.clone();
    let port = serve(Arc::new(move |request: Request<Bytes>| {
        let action = request
            .headers()
            .get("soapaction")
            .map(|v| v.to_str().unwrap().to_owned())
            .unwrap_or_default();
        let body = String::from_utf8(request.body().to_vec()).unwrap();
        log.lock().unwrap().push((action, body.clone()));
        let code = if body.contains("REJECT") {
            "soap:Client"
        } else if body.contains("BUSY") {
            "soap:Server"
        } else {
            return xml(
                StatusCode::OK,
                r#"<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/"><soap:Body><r:Accepted xmlns:r="urn:r">ok</r:Accepted></soap:Body></soap:Envelope>"#,
            );
        };
        xml(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!(
                r#"<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/"><soap:Body><soap:Fault><faultcode>{code}</faultcode><faultstring>synthetic fault</faultstring></soap:Fault></soap:Body></soap:Envelope>"#
            ),
        )
    }))
    .await;
    let sender = destination(
        "soap",
        &format!(
            "      url: 'http://127.0.0.1:{port}/ws'
      action: urn:lab:Submit
      ws_security: {{username: lab, password: synthetic-ws-password}}"
        ),
    );
    let answer = sender
        .send(&delivery(
            1,
            b"<m:Order xmlns:m=\"urn:m\">1</m:Order>",
            DataType::Xml,
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(answer, br#"<r:Accepted xmlns:r="urn:r">ok</r:Accepted>"#);
    let (action, body) = seen.lock().unwrap()[0].clone();
    assert_eq!(action, "\"urn:lab:Submit\"");
    assert!(
        body.contains("<wsse:Username>lab</wsse:Username>"),
        "{body}"
    );
    assert!(body.contains("#PasswordDigest\">"), "{body}");
    assert!(!body.contains("synthetic-ws-password"), "{body}");
    assert!(
        body.contains("<soap:Body><m:Order xmlns:m=\"urn:m\">1</m:Order></soap:Body>"),
        "{body}"
    );

    let error = sender
        .send(&delivery(
            2,
            b"<m:Order xmlns:m=\"urn:m\">REJECT</m:Order>",
            DataType::Xml,
        ))
        .await
        .unwrap_err();
    assert!(
        error.permanent && error.to_string().contains("synthetic fault"),
        "{error}"
    );
    let error = sender
        .send(&delivery(
            3,
            b"<m:Order xmlns:m=\"urn:m\">BUSY</m:Order>",
            DataType::Xml,
        ))
        .await
        .unwrap_err();
    assert!(!error.permanent, "{error}");
}

#[tokio::test(flavor = "multi_thread")]
async fn uses_and_refreshes_oauth2_tokens() {
    let issued = Arc::new(AtomicUsize::new(0));
    let counter = issued.clone();
    let token_port = serve(Arc::new(move |request: Request<Bytes>| {
        let body = String::from_utf8(request.body().to_vec()).unwrap();
        let authorization = request
            .headers()
            .get("authorization")
            .map(|v| v.to_str().unwrap().to_owned())
            .unwrap_or_default();
        // Basic base64("lab-client:synthetic-client-secret")
        if !body.contains("grant_type=client_credentials")
            || authorization != "Basic bGFiLWNsaWVudDpzeW50aGV0aWMtY2xpZW50LXNlY3JldA=="
        {
            return xml(StatusCode::UNAUTHORIZED, r#"{"error":"invalid_client"}"#);
        }
        let n = counter.fetch_add(1, Ordering::SeqCst) + 1;
        xml(
            StatusCode::OK,
            &format!(r#"{{"access_token":"token-{n}","token_type":"Bearer","expires_in":3600}}"#),
        )
    }))
    .await;
    let revoked = Arc::new(Mutex::new(String::new()));
    let revoked_token = revoked.clone();
    let service_port = serve(Arc::new(move |request: Request<Bytes>| {
        let authorization = request
            .headers()
            .get("authorization")
            .map(|v| v.to_str().unwrap().to_owned())
            .unwrap_or_default();
        if !authorization.starts_with("Bearer token-")
            || authorization == format!("Bearer {}", revoked_token.lock().unwrap())
        {
            return xml(StatusCode::UNAUTHORIZED, "");
        }
        xml(
            StatusCode::OK,
            r#"<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/"><soap:Body><ok/></soap:Body></soap:Envelope>"#,
        )
    }))
    .await;
    let sender = destination(
        "soap",
        &format!(
            "      url: 'http://127.0.0.1:{service_port}/ws'
      oauth2:
        token_url: 'http://127.0.0.1:{token_port}/token'
        client_id: lab-client
        client_secret: synthetic-client-secret
        scope: results.write"
        ),
    );
    let body = b"<m:Order xmlns:m=\"urn:m\">1</m:Order>";
    sender
        .send(&delivery(1, body, DataType::Xml))
        .await
        .unwrap();
    sender
        .send(&delivery(2, body, DataType::Xml))
        .await
        .unwrap();
    assert_eq!(issued.load(Ordering::SeqCst), 1, "the token is cached");

    // A revoked token is discarded after the 401 and a new one is fetched.
    *revoked.lock().unwrap() = "token-1".into();
    let error = sender
        .send(&delivery(3, body, DataType::Xml))
        .await
        .unwrap_err();
    assert!(
        !error.permanent && error.to_string().contains("OAuth"),
        "{error}"
    );
    sender
        .send(&delivery(3, body, DataType::Xml))
        .await
        .unwrap();
    assert_eq!(issued.load(Ordering::SeqCst), 2);
}
