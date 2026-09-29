//! S3-compatible object storage (Amazon S3, MinIO, Ceph, and others): a
//! poller for objects under a prefix and an object writer, over HTTPS with
//! AWS Signature Version 4 ([`sigv4`]).
//!
//! Connection settings of type `s3` (source and destination), in addition
//! to the shared polling and writing settings (see the crate README), where
//! `directory` is the key prefix (a "folder"; empty for the bucket root):
//!
//! | Setting | Default | Meaning |
//! |---|---|---|
//! | `endpoint` | required | Service URL, for example `https://s3.eu-central-1.amazonaws.com` or `https://minio.lab.internal:9000` |
//! | `region` | `us-east-1` | Signing region |
//! | `bucket` | required | Bucket name |
//! | `path_style` | `false` | Address the bucket in the path (`endpoint/bucket/key`, usual for MinIO) instead of the host name |
//! | `access_key_id_env` | none | Environment variable holding the access key id |
//! | `secret_access_key_env` | none | Environment variable holding the secret access key |
//! | `session_token_env` | none | Environment variable holding a session token (temporary credentials) |
//! | `access_key_id`, `secret_access_key` | none | The keys inline (discouraged) |
//! | `tls` | system roots | TLS settings; see [`oxim_connectors::tls`] |
//! | `timeout` | `60s` | Limit for each request |
//!
//! Objects are listed with `ListObjectsV2` one level below the prefix
//! (`delimiter=/`). `after: move` copies the object to the processed
//! prefix and deletes the original. Writes are single `PUT` requests, which
//! S3 applies atomically, so no temporary key is used. Payloads are always
//! signed.

pub mod sigv4;

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http::{Method, Request, StatusCode, Uri};
use http_body_util::{BodyExt, Full};
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use oxim_connectors::tls::{ClientTlsSettings, client_config};
use oxim_core::config::DurationText;
use oxim_core::{
    DestinationConfig, DestinationConnector, EngineError, Registry, SourceConfig, SourceConnector,
    async_trait,
};
use oxim_model::ClinicalDateTime;
use serde::Deserialize;

use self::sigv4::{Credentials, Scope, SignedRequest, sha256_hex, uri_encode};
use crate::remote::{
    Connect, PollingSource, RemoteDestination, RemoteEntry, Session, poll_settings, write_settings,
};
use crate::util::{check_secret, compact_utc, now, parse, secret};
use crate::xml;

fn default_region() -> String {
    "us-east-1".to_owned()
}

fn default_timeout() -> DurationText {
    DurationText(Duration::from_secs(60))
}

/// Connection settings of the `s3` connectors.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct S3Settings {
    /// Service URL.
    pub endpoint: String,
    /// Signing region.
    #[serde(default = "default_region")]
    pub region: String,
    /// Bucket name.
    pub bucket: String,
    /// Path-style addressing.
    #[serde(default)]
    pub path_style: bool,
    /// Environment variable holding the access key id.
    #[serde(default)]
    pub access_key_id_env: Option<String>,
    /// Environment variable holding the secret access key.
    #[serde(default)]
    pub secret_access_key_env: Option<String>,
    /// Environment variable holding a session token.
    #[serde(default)]
    pub session_token_env: Option<String>,
    /// The access key id inline (discouraged).
    #[serde(default)]
    pub access_key_id: Option<String>,
    /// The secret access key inline (discouraged).
    #[serde(default)]
    pub secret_access_key: Option<String>,
    /// TLS settings.
    #[serde(default)]
    pub tls: Option<ClientTlsSettings>,
    /// Limit for each request.
    #[serde(default = "default_timeout")]
    pub timeout: DurationText,
}

struct Inner {
    settings: S3Settings,
    scheme: String,
    /// `host[:port]` requests go to.
    authority: String,
    /// Path prefix before keys: `/bucket` for path-style, empty otherwise.
    base_path: String,
    client: Client<HttpsConnector<HttpConnector>, Full<Bytes>>,
}

/// The bucket and credentials, validated at deploy time.
#[derive(Clone)]
pub(crate) struct S3Connect {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for S3Connect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Connect")
            .field("endpoint", &self.inner.settings.endpoint)
            .field("bucket", &self.inner.settings.bucket)
            .finish_non_exhaustive()
    }
}

fn valid_bucket(bucket: &str) -> bool {
    (3..=63).contains(&bucket.len())
        && bucket
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
        && !bucket.starts_with(['-', '.'])
        && !bucket.ends_with(['-', '.'])
}

impl S3Connect {
    fn new(settings: S3Settings) -> Result<Self, EngineError> {
        let error = |message: String| EngineError::Config(format!("s3: {message}"));
        let uri: Uri = settings
            .endpoint
            .parse()
            .map_err(|e| error(format!("invalid endpoint {:?}: {e}", settings.endpoint)))?;
        let scheme = match uri.scheme_str() {
            Some(scheme @ ("http" | "https")) => scheme.to_owned(),
            _ => {
                return Err(error(format!(
                    "endpoint {:?} must be an http:// or https:// URL",
                    settings.endpoint
                )));
            }
        };
        let authority = uri
            .authority()
            .ok_or_else(|| error(format!("endpoint {:?} has no host", settings.endpoint)))?
            .to_string();
        if !matches!(uri.path(), "" | "/") {
            return Err(error("endpoint must not contain a path".into()));
        }
        if !valid_bucket(&settings.bucket) {
            return Err(error(format!("invalid bucket name {:?}", settings.bucket)));
        }
        if !settings.path_style && settings.bucket.contains('.') && scheme == "https" {
            return Err(error(
                "bucket names with dots need path_style: true over HTTPS".into(),
            ));
        }
        check_secret(
            settings.access_key_id.as_ref(),
            settings.access_key_id_env.as_ref(),
            "access key id",
            "s3",
        )?;
        check_secret(
            settings.secret_access_key.as_ref(),
            settings.secret_access_key_env.as_ref(),
            "secret access key",
            "s3",
        )?;
        let (authority, base_path) = if settings.path_style {
            (authority, format!("/{}", settings.bucket))
        } else {
            (format!("{}.{authority}", settings.bucket), String::new())
        };
        let tls = settings.tls.clone().unwrap_or_default();
        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(client_config(&tls)?)
            .https_or_http()
            .enable_http1()
            .build();
        Ok(Self {
            inner: Arc::new(Inner {
                settings,
                scheme,
                authority,
                base_path,
                client: Client::builder(TokioExecutor::new()).build(connector),
            }),
        })
    }
}

impl Inner {
    fn credentials(&self) -> Result<Credentials, String> {
        let settings = &self.settings;
        let access_key_id = secret(
            settings.access_key_id.as_ref(),
            settings.access_key_id_env.as_ref(),
            "access key id",
        )?
        .ok_or("no access key id: set access_key_id_env")?;
        let secret_access_key = secret(
            settings.secret_access_key.as_ref(),
            settings.secret_access_key_env.as_ref(),
            "secret access key",
        )?
        .ok_or("no secret access key: set secret_access_key_env")?;
        let session_token = secret(None, settings.session_token_env.as_ref(), "session token")?;
        Ok(Credentials {
            access_key_id,
            secret_access_key,
            session_token,
        })
    }

    fn path(&self, key: &str) -> String {
        format!("{}/{}", self.base_path, uri_encode(key, false))
    }

    /// Sends one signed request and returns the status and body.
    async fn call(
        &self,
        method: Method,
        key: &str,
        query: &[(String, String)],
        body: Vec<u8>,
        extra: &[(String, String)],
    ) -> Result<(StatusCode, Bytes), String> {
        let credentials = self.credentials()?;
        let amz_date = compact_utc(now());
        let payload_hash = sha256_hex(&body);
        let path = self.path(key);
        let mut headers = vec![
            ("host".to_owned(), self.authority.clone()),
            ("x-amz-content-sha256".to_owned(), payload_hash.clone()),
            ("x-amz-date".to_owned(), amz_date.clone()),
        ];
        if let Some(token) = &credentials.session_token {
            headers.push(("x-amz-security-token".to_owned(), token.clone()));
        }
        headers.extend(extra.iter().cloned());
        let request = SignedRequest {
            method: method.as_str(),
            path: &path,
            query,
            headers: &headers,
            payload_hash: &payload_hash,
        };
        let scope = Scope {
            region: &self.settings.region,
            service: "s3",
            amz_date: &amz_date,
        };
        let authorization = sigv4::authorization(&request, &credentials, &scope);
        let query_string = sigv4::canonical_query(query);
        let uri = if query_string.is_empty() {
            format!("{}://{}{path}", self.scheme, self.authority)
        } else {
            format!("{}://{}{path}?{query_string}", self.scheme, self.authority)
        };
        let mut builder = Request::builder().method(method.clone()).uri(&uri);
        for (name, value) in &headers {
            if name != "host" {
                builder = builder.header(name.as_str(), value.as_str());
            }
        }
        let request = builder
            .header(http::header::AUTHORIZATION, authorization)
            .body(Full::new(Bytes::from(body)))
            .map_err(|e| format!("cannot build the request: {e}"))?;
        let work = async {
            let response = self
                .client
                .request(request)
                .await
                .map_err(|e| format!("{method} {uri} failed: {e}"))?;
            let status = response.status();
            let body = response
                .into_body()
                .collect()
                .await
                .map_err(|e| format!("reading the response of {method} {uri} failed: {e}"))?
                .to_bytes();
            Ok((status, body))
        };
        tokio::time::timeout(self.settings.timeout.0, work)
            .await
            .map_err(|_| format!("{method} {uri} timed out"))?
    }

    async fn expect_success(
        &self,
        method: Method,
        key: &str,
        query: &[(String, String)],
        body: Vec<u8>,
        extra: &[(String, String)],
    ) -> Result<Bytes, String> {
        let (status, body) = self.call(method.clone(), key, query, body, extra).await?;
        if status.is_success() {
            // CopyObject may answer 200 with an error document.
            if method == Method::PUT && body.windows(7).any(|w| w == b"<Error>") {
                return Err(format!("{method} {key}: {}", error_text(&body)));
            }
            Ok(body)
        } else {
            Err(format!(
                "{method} {key}: HTTP {status}: {}",
                error_text(&body)
            ))
        }
    }
}

/// The `Code: Message` of an S3 error document, or the start of the body.
fn error_text(body: &[u8]) -> String {
    let (mut code, mut message) = (String::new(), String::new());
    let _ = xml::walk(body, |path, text| match path.last().map(String::as_str) {
        Some("Code") => code = text.to_owned(),
        Some("Message") => message = text.to_owned(),
        _ => {}
    });
    if code.is_empty() {
        String::from_utf8_lossy(&body[..body.len().min(200)]).into_owned()
    } else {
        format!("{code}: {message}")
    }
}

/// One page of a `ListObjectsV2` answer.
#[derive(Debug, Default, PartialEq)]
struct Listing {
    objects: Vec<(String, u64, Option<i64>)>,
    next: Option<String>,
}

fn parse_listing(body: &[u8]) -> Result<Listing, String> {
    let mut listing = Listing::default();
    let mut object = (String::new(), 0u64, None);
    let mut truncated = false;
    let mut token = None;
    xml::walk(body, |path, text| {
        let parent = path.len().checked_sub(2).and_then(|i| path.get(i));
        match (parent.map(String::as_str), path.last().map(String::as_str)) {
            (Some("Contents"), Some("Key")) => object.0 = text.to_owned(),
            (Some("Contents"), Some("Size")) => object.1 = text.trim().parse().unwrap_or(0),
            (Some("Contents"), Some("LastModified")) => {
                object.2 = ClinicalDateTime::parse_iso(text.trim())
                    .ok()
                    .and_then(|t| t.to_timestamp(0))
                    .map(|t| t.unix_millis() / 1000);
            }
            (Some("ListBucketResult"), Some("Contents")) => {
                listing.objects.push(std::mem::take(&mut object));
            }
            (_, Some("IsTruncated")) => truncated = text.trim() == "true",
            (_, Some("NextContinuationToken")) => token = Some(text.to_owned()),
            _ => {}
        }
    })
    .map_err(|e| format!("invalid listing: {e}"))?;
    listing.next = if truncated { token } else { None };
    Ok(listing)
}

/// Requests to one bucket; S3 has no connection state.
struct S3Session {
    inner: Arc<Inner>,
}

#[async_trait]
impl Session for S3Session {
    async fn list(&mut self, directory: &str) -> Result<Vec<RemoteEntry>, String> {
        let prefix = match directory.trim_matches('/') {
            "" => String::new(),
            directory => format!("{directory}/"),
        };
        let mut entries = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut query = vec![
                ("list-type".to_owned(), "2".to_owned()),
                ("delimiter".to_owned(), "/".to_owned()),
                ("prefix".to_owned(), prefix.clone()),
            ];
            if let Some(token) = &token {
                query.push(("continuation-token".to_owned(), token.clone()));
            }
            let body = self
                .inner
                .expect_success(Method::GET, "", &query, Vec::new(), &[])
                .await?;
            let listing = parse_listing(&body)?;
            for (key, size, modified) in listing.objects {
                let Some(name) = key.strip_prefix(&prefix) else {
                    continue;
                };
                if !name.is_empty() && !name.contains('/') {
                    entries.push(RemoteEntry {
                        name: name.to_owned(),
                        size,
                        modified,
                    });
                }
            }
            match listing.next {
                Some(next) => token = Some(next),
                None => return Ok(entries),
            }
        }
    }

    async fn read(&mut self, path: &str) -> Result<Vec<u8>, String> {
        self.inner
            .expect_success(Method::GET, path, &[], Vec::new(), &[])
            .await
            .map(|body| body.to_vec())
    }

    async fn write(&mut self, path: &str, data: &[u8]) -> Result<(), String> {
        let extra = [(
            "content-type".to_owned(),
            "application/octet-stream".to_owned(),
        )];
        self.inner
            .expect_success(Method::PUT, path, &[], data.to_vec(), &extra)
            .await
            .map(|_| ())
    }

    async fn remove(&mut self, path: &str) -> Result<(), String> {
        self.inner
            .expect_success(Method::DELETE, path, &[], Vec::new(), &[])
            .await
            .map(|_| ())
    }

    async fn rename(&mut self, from: &str, to: &str) -> Result<(), String> {
        let source = format!(
            "/{}/{}",
            self.inner.settings.bucket,
            uri_encode(from, false)
        );
        let extra = [("x-amz-copy-source".to_owned(), source)];
        self.inner
            .expect_success(Method::PUT, to, &[], Vec::new(), &extra)
            .await?;
        self.remove(from).await
    }

    async fn exists(&mut self, path: &str) -> Result<bool, String> {
        let (status, _) = self
            .inner
            .call(Method::HEAD, path, &[], Vec::new(), &[])
            .await?;
        match status {
            status if status.is_success() => Ok(true),
            StatusCode::NOT_FOUND => Ok(false),
            StatusCode::FORBIDDEN => Err(format!(
                "HEAD {path}: HTTP 403 Forbidden: access denied or signature mismatch \
                 (check the keys, the region and the permissions)"
            )),
            status => Err(format!("HEAD {path}: HTTP {status}")),
        }
    }

    async fn create_dir_all(&mut self, _directory: &str) -> Result<(), String> {
        Ok(())
    }

    fn atomic_write(&self) -> bool {
        true
    }
}

#[async_trait]
impl Connect for S3Connect {
    async fn connect(&self) -> Result<Box<dyn Session>, String> {
        // Check the credentials now so a missing variable is reported at
        // once instead of with every request.
        self.inner.credentials()?;
        Ok(Box::new(S3Session {
            inner: self.inner.clone(),
        }))
    }

    fn location(&self) -> String {
        format!(
            "s3://{}@{}",
            self.inner.settings.bucket, self.inner.settings.endpoint
        )
    }
}

/// Registers the `s3` source and destination.
pub fn register(registry: &mut Registry) {
    registry
        .add_source("s3", |config: &SourceConfig| {
            let (poll, rest) = poll_settings(&config.settings, "s3 source")?;
            let connect = S3Connect::new(parse(&rest, "s3 source")?)?;
            Ok(Arc::new(PollingSource::new(connect, poll, "s3")) as Arc<dyn SourceConnector>)
        })
        .add_destination("s3", |config: &DestinationConfig| {
            let (write, rest) = write_settings(&config.settings, "s3 destination")?;
            let connect = S3Connect::new(parse(&rest, "s3 destination")?)?;
            Ok(Arc::new(RemoteDestination::new(connect, write)) as Arc<dyn DestinationConnector>)
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_listings() {
        let body = br#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">
  <Name>lab</Name><Prefix>in/</Prefix><KeyCount>2</KeyCount>
  <IsTruncated>true</IsTruncated><NextContinuationToken>abc==</NextContinuationToken>
  <Contents><Key>in/a&amp;b.hl7</Key><LastModified>2026-09-29T12:00:00.000Z</LastModified><Size>42</Size></Contents>
  <Contents><Key>in/c.hl7</Key><Size>7</Size></Contents>
  <CommonPrefixes><Prefix>in/sub/</Prefix></CommonPrefixes>
</ListBucketResult>"#;
        let listing = parse_listing(body).unwrap();
        assert_eq!(listing.next.as_deref(), Some("abc=="));
        assert_eq!(listing.objects.len(), 2);
        assert_eq!(listing.objects[0].0, "in/a&b.hl7");
        assert_eq!(listing.objects[0].1, 42);
        assert_eq!(listing.objects[0].2, Some(1_790_683_200));
        assert_eq!(listing.objects[1], ("in/c.hl7".to_owned(), 7, None));
    }

    #[test]
    fn reads_error_documents() {
        assert_eq!(
            error_text(
                b"<Error><Code>NoSuchKey</Code><Message>The key does not exist</Message></Error>"
            ),
            "NoSuchKey: The key does not exist"
        );
    }

    fn connect(extra: &str) -> Result<S3Connect, EngineError> {
        S3Connect::new(
            serde_json::from_str(&format!(
                r#"{{"endpoint":"https://s3.eu-central-1.amazonaws.com","bucket":"lab-results"{extra}}}"#
            ))
            .unwrap(),
        )
    }

    #[test]
    fn addresses_buckets() {
        let virtual_hosted = connect("").unwrap();
        assert_eq!(
            virtual_hosted.inner.authority,
            "lab-results.s3.eu-central-1.amazonaws.com"
        );
        assert_eq!(virtual_hosted.inner.path("in/a b.hl7"), "/in/a%20b.hl7");
        let path_style = connect(r#","path_style":true"#).unwrap();
        assert_eq!(path_style.inner.authority, "s3.eu-central-1.amazonaws.com");
        assert_eq!(path_style.inner.path("x"), "/lab-results/x");
        assert!(
            S3Connect::new(
                serde_json::from_str(
                    r#"{"endpoint":"https://s3.example.org","bucket":"Bad_Name"}"#
                )
                .unwrap()
            )
            .is_err()
        );
        assert!(connect(r#","access_key_id":"a","access_key_id_env":"B""#).is_err());
    }
}
