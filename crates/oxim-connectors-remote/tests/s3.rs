//! S3 source and destination against an in-process S3-compatible endpoint
//! that checks every Signature Version 4 signature.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use common::{Recorder, channel, delivery, destination, engine, wait_until};
use http::{Method, Request, Response, StatusCode};
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use oxim_connectors_remote::s3::sigv4::{self, Credentials, Scope, SignedRequest};
use oxim_model::DataType;
use tokio::net::TcpListener;

const ACCESS_KEY: &str = "SYNTHETICACCESSKEY01";
const SECRET_KEY: &str = "synthetic/secret+key/for/tests/only";
const BUCKET: &str = "lab-results";

type Objects = Arc<Mutex<BTreeMap<String, Vec<u8>>>>;

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(byte) = u8::from_str_radix(&text[i + 1..i + 3], 16)
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
        i += 1;
    }
    String::from_utf8(out).unwrap()
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn reply(status: StatusCode, body: impl Into<Bytes>) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(body.into()));
    *response.status_mut() = status;
    response
}

fn error(status: StatusCode, code: &str) -> Response<Full<Bytes>> {
    reply(
        status,
        format!("<Error><Code>{code}</Code><Message>synthetic test endpoint</Message></Error>"),
    )
}

/// Recomputes the signature of a request; `None` when it does not match.
fn verify(parts: &http::request::Parts, body: &[u8]) -> Result<(), &'static str> {
    let authorization = parts
        .headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .ok_or("AccessDenied")?;
    let fields: BTreeMap<&str, &str> = authorization
        .strip_prefix("AWS4-HMAC-SHA256 ")
        .ok_or("AccessDenied")?
        .split(", ")
        .filter_map(|field| field.split_once('='))
        .collect();
    let credential: Vec<&str> = fields
        .get("Credential")
        .ok_or("AccessDenied")?
        .split('/')
        .collect();
    if credential.first() != Some(&ACCESS_KEY) {
        return Err("InvalidAccessKeyId");
    }
    let payload_hash = parts
        .headers
        .get("x-amz-content-sha256")
        .and_then(|v| v.to_str().ok())
        .ok_or("AccessDenied")?;
    if payload_hash != sigv4::sha256_hex(body) {
        return Err("XAmzContentSHA256Mismatch");
    }
    let headers: Vec<(String, String)> = fields
        .get("SignedHeaders")
        .ok_or("AccessDenied")?
        .split(';')
        .map(|name| {
            let value = parts
                .headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_owned();
            (name.to_owned(), value)
        })
        .collect();
    let query: Vec<(String, String)> = parts
        .uri
        .query()
        .unwrap_or_default()
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (percent_decode(key), percent_decode(value))
        })
        .collect();
    let amz_date = parts
        .headers
        .get("x-amz-date")
        .and_then(|v| v.to_str().ok())
        .ok_or("AccessDenied")?;
    let request = SignedRequest {
        method: parts.method.as_str(),
        path: parts.uri.path(),
        query: &query,
        headers: &headers,
        payload_hash,
    };
    let credentials = Credentials {
        access_key_id: ACCESS_KEY.into(),
        secret_access_key: SECRET_KEY.into(),
        session_token: None,
    };
    let scope = Scope {
        region: credential.get(2).copied().unwrap_or_default(),
        service: "s3",
        amz_date,
    };
    let (expected, _) = sigv4::signature(&request, &credentials, &scope);
    if fields.get("Signature") == Some(&expected.as_str()) {
        Ok(())
    } else {
        Err("SignatureDoesNotMatch")
    }
}

async fn handle(objects: Objects, request: Request<Incoming>) -> Response<Full<Bytes>> {
    let (parts, body) = request.into_parts();
    let body = body.collect().await.unwrap().to_bytes();
    if let Err(code) = verify(&parts, &body) {
        return error(StatusCode::FORBIDDEN, code);
    }
    let path = percent_decode(parts.uri.path());
    let Some(rest) = path.strip_prefix(&format!("/{BUCKET}")) else {
        return error(StatusCode::NOT_FOUND, "NoSuchBucket");
    };
    let key = rest.trim_start_matches('/').to_owned();
    let mut objects = objects.lock().unwrap();
    match (parts.method.clone(), key.is_empty()) {
        (Method::GET, true) => {
            let query: BTreeMap<String, String> = parts
                .uri
                .query()
                .unwrap_or_default()
                .split('&')
                .filter_map(|pair| pair.split_once('='))
                .map(|(k, v)| (percent_decode(k), percent_decode(v)))
                .collect();
            let prefix = query.get("prefix").cloned().unwrap_or_default();
            let mut xml = String::from(
                r#"<?xml version="1.0" encoding="UTF-8"?><ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/"><IsTruncated>false</IsTruncated>"#,
            );
            for (key, data) in objects.iter() {
                if let Some(name) = key.strip_prefix(&prefix)
                    && !name.contains('/')
                {
                    xml.push_str(&format!(
                        "<Contents><Key>{}</Key><LastModified>2026-09-29T12:00:00.000Z</LastModified><Size>{}</Size></Contents>",
                        xml_escape(key),
                        data.len()
                    ));
                }
            }
            xml.push_str("</ListBucketResult>");
            reply(StatusCode::OK, xml)
        }
        (Method::GET, false) => match objects.get(&key) {
            Some(data) => reply(StatusCode::OK, data.clone()),
            None => error(StatusCode::NOT_FOUND, "NoSuchKey"),
        },
        (Method::HEAD, false) => match objects.get(&key) {
            Some(_) => reply(StatusCode::OK, Bytes::new()),
            None => reply(StatusCode::NOT_FOUND, Bytes::new()),
        },
        (Method::PUT, false) => {
            if let Some(source) = parts.headers.get("x-amz-copy-source") {
                let source = percent_decode(source.to_str().unwrap());
                let source_key = source
                    .strip_prefix(&format!("/{BUCKET}/"))
                    .unwrap_or_default()
                    .to_owned();
                match objects.get(&source_key).cloned() {
                    Some(data) => {
                        objects.insert(key, data);
                        reply(StatusCode::OK, "<CopyObjectResult/>")
                    }
                    None => error(StatusCode::NOT_FOUND, "NoSuchKey"),
                }
            } else {
                objects.insert(key, body.to_vec());
                reply(StatusCode::OK, Bytes::new())
            }
        }
        (Method::DELETE, false) => {
            objects.remove(&key);
            reply(StatusCode::NO_CONTENT, Bytes::new())
        }
        _ => error(StatusCode::METHOD_NOT_ALLOWED, "MethodNotAllowed"),
    }
}

async fn start() -> (u16, Objects) {
    let objects: Objects = Arc::default();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let shared = objects.clone();
    tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                return;
            };
            let objects = shared.clone();
            tokio::spawn(async move {
                let service = service_fn(move |request| {
                    let objects = objects.clone();
                    async move { Ok::<_, Infallible>(handle(objects, request).await) }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(socket), service)
                    .await;
            });
        }
    });
    (port, objects)
}

fn connection(port: u16, secret: &str) -> String {
    format!(
        "endpoint: 'http://127.0.0.1:{port}'
    region: eu-central-1
    bucket: {BUCKET}
    path_style: true
    access_key_id: {ACCESS_KEY}
    secret_access_key: '{secret}'"
    )
}

const RESULT: &[u8] =
    b"MSH|^~\\&|ANALYZER|LAB|LIS|HOSP|20260929120000||ORU^R01|B1|P|2.5.1\rPID|1||SYNTH-3\r";

#[tokio::test(flavor = "multi_thread")]
async fn polls_a_prefix_and_moves_stored_objects() {
    let (port, objects) = start().await;
    objects
        .lock()
        .unwrap()
        .insert("in/result 1.hl7".into(), RESULT.to_vec());
    objects
        .lock()
        .unwrap()
        .insert("in/nested/skip.hl7".into(), b"not listed".to_vec());
    let recorder = Arc::new(Recorder::default());
    let engine = engine(recorder.clone()).await;
    engine
        .deploy(channel(&format!(
            "  type: s3
  data_type: hl7v2
  settings:
    {}
    directory: in
    poll_interval: 100ms
    processed_directory: done",
            connection(port, SECRET_KEY)
        )))
        .await
        .unwrap();
    wait_until("the object is stored", || recorder.payloads().len() == 1).await;
    assert_eq!(recorder.payloads()[0], RESULT);
    wait_until("the object is moved", || {
        let objects = objects.lock().unwrap();
        objects.contains_key("done/result 1.hl7") && !objects.contains_key("in/result 1.hl7")
    })
    .await;
    assert!(objects.lock().unwrap().contains_key("in/nested/skip.hl7"));
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn writes_objects_and_reports_signature_errors() {
    let (port, objects) = start().await;
    let settings = |secret: &str| {
        format!(
            "      {}
      directory: out/lis
      filename: '{{message_id}}.{{extension}}'",
            connection(port, secret).replace("\n    ", "\n      ")
        )
    };
    let sender = destination("s3", &settings(SECRET_KEY));
    sender
        .send(&delivery(1, RESULT, DataType::Hl7V2))
        .await
        .unwrap();
    sender
        .send(&delivery(1, RESULT, DataType::Hl7V2))
        .await
        .unwrap();
    {
        let objects = objects.lock().unwrap();
        let written: Vec<&String> = objects
            .keys()
            .filter(|k| k.starts_with("out/lis/"))
            .collect();
        assert_eq!(written.len(), 1, "{written:?}");
        assert!(written[0].ends_with(".hl7"));
        assert_eq!(objects[written[0]], RESULT);
    }
    let error = sender
        .send(&delivery(1, b"different", DataType::Hl7V2))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("different content"), "{error}");

    let wrong = destination("s3", &settings("not-the-secret"));
    let error = wrong
        .send(&delivery(2, RESULT, DataType::Hl7V2))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("signature mismatch"), "{error}");
    assert!(
        !objects
            .lock()
            .unwrap()
            .keys()
            .any(|k| k.contains("01M3250V02")),
        "a rejected request must not store anything"
    );
}
