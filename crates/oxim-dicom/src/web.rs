//! HTTP helpers shared by the DICOMweb destinations.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use bytes::Bytes;
use http::Uri;
use http::header::{AUTHORIZATION, HeaderName, HeaderValue};
use http_body_util::Full;
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use oxim_core::EngineError;
use rustls_pki_types::CertificateDer;
use rustls_pki_types::pem::PemObject;
use tracing::debug;

/// An HTTP(S) client.
pub(crate) type HttpClient = Client<HttpsConnector<HttpConnector>, Full<Bytes>>;

fn tls_config(ca_file: Option<&PathBuf>) -> Result<rustls::ClientConfig, EngineError> {
    let mut roots = rustls::RootCertStore::empty();
    let native = rustls_native_certs::load_native_certs();
    for error in &native.errors {
        debug!(%error, "cannot load a system certificate");
    }
    let (_added, _ignored) = roots.add_parsable_certificates(native.certs);
    if let Some(path) = ca_file {
        let certificates = CertificateDer::pem_file_iter(path)
            .map_err(|e| EngineError::Config(format!("cannot read {}: {e}", path.display())))?;
        for certificate in certificates {
            let certificate = certificate.map_err(|e| {
                EngineError::Config(format!("invalid certificate in {}: {e}", path.display()))
            })?;
            roots.add(certificate).map_err(|e| {
                EngineError::Config(format!("invalid certificate in {}: {e}", path.display()))
            })?;
        }
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    Ok(rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| EngineError::Config(format!("TLS configuration: {e}")))?
        .with_root_certificates(roots)
        .with_no_client_auth())
}

/// An HTTP client trusting the system's certificate authorities plus
/// `ca_file`.
pub(crate) fn client(ca_file: Option<&PathBuf>) -> Result<HttpClient, EngineError> {
    let connector = hyper_rustls::HttpsConnectorBuilder::new()
        .with_tls_config(tls_config(ca_file)?)
        .https_or_http()
        .enable_http1()
        .build();
    Ok(Client::builder(TokioExecutor::new()).build(connector))
}

/// The base URL of a DICOMweb service, without a trailing slash.
pub(crate) fn base_url(what: &str, url: &str) -> Result<String, EngineError> {
    let base = url.trim().trim_end_matches('/').to_owned();
    let uri: Uri = base
        .parse()
        .map_err(|e| EngineError::Config(format!("{what}: invalid url {url:?}: {e}")))?;
    if !matches!(uri.scheme_str(), Some("http" | "https")) || uri.host().is_none() {
        return Err(EngineError::Config(format!(
            "{what}: url {url:?} must be an absolute http:// or https:// URL"
        )));
    }
    Ok(base)
}

/// Extra headers plus `Authorization: Bearer` from a token or an
/// environment variable read at deploy time.
pub(crate) fn headers(
    what: &str,
    extra: &BTreeMap<String, String>,
    bearer_token: Option<&String>,
    bearer_token_env: Option<&String>,
) -> Result<Vec<(HeaderName, HeaderValue)>, EngineError> {
    let config = |message: String| EngineError::Config(format!("{what}: {message}"));
    let mut headers = extra
        .iter()
        .map(|(name, value)| {
            Ok((
                HeaderName::from_bytes(name.as_bytes())
                    .map_err(|e| config(format!("invalid header name {name:?}: {e}")))?,
                HeaderValue::from_str(value)
                    .map_err(|e| config(format!("invalid value for header {name}: {e}")))?,
            ))
        })
        .collect::<Result<Vec<_>, EngineError>>()?;
    let token = match (bearer_token, bearer_token_env) {
        (Some(_), Some(_)) => {
            return Err(config(
                "set bearer_token or bearer_token_env, not both".into(),
            ));
        }
        (Some(token), None) => Some(token.clone()),
        (None, Some(variable)) => Some(
            std::env::var(variable)
                .map_err(|_| config(format!("environment variable {variable} is not set")))?,
        ),
        (None, None) => None,
    };
    if let Some(token) = token {
        let mut value = HeaderValue::from_str(&format!("Bearer {}", token.trim()))
            .map_err(|_| config("the bearer token is not a valid header value".into()))?;
        value.set_sensitive(true);
        headers.push((AUTHORIZATION, value));
    }
    Ok(headers)
}

/// The value of a `name=value` parameter of a media type, unquoted.
pub(crate) fn media_parameter<'a>(content_type: &'a str, name: &str) -> Option<&'a str> {
    content_type.split(';').skip(1).find_map(|parameter| {
        let (key, value) = parameter.split_once('=')?;
        (key.trim().eq_ignore_ascii_case(name)).then(|| value.trim().trim_matches('"'))
    })
}

/// The bodies of the parts of a `multipart/related` body.
pub(crate) fn multipart_parts<'a>(body: &'a [u8], boundary: &str) -> Result<Vec<&'a [u8]>, String> {
    let delimiter = format!("--{boundary}");
    let finder = memchr::memmem::Finder::new(delimiter.as_bytes());
    let positions: Vec<usize> = finder.find_iter(body).collect();
    if positions.is_empty() {
        return Err("the multipart body has no boundary".into());
    }
    let mut parts = Vec::new();
    for window in positions.windows(2) {
        let start = window[0] + delimiter.len();
        let end = window[1];
        let part = &body[start..end];
        // Each part: CRLF, headers, CRLF CRLF, content, CRLF.
        let part = part.strip_prefix(b"\r\n").unwrap_or(part);
        let Some(split) = memchr::memmem::find(part, b"\r\n\r\n") else {
            return Err("a multipart part has no header end".into());
        };
        let content = &part[split + 4..];
        let content = content.strip_suffix(b"\r\n").unwrap_or(content);
        parts.push(content);
    }
    let last = positions[positions.len() - 1] + delimiter.len();
    if !body[last..].starts_with(b"--") {
        return Err("the multipart body has no closing boundary".into());
    }
    Ok(parts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_multipart_bodies() {
        let body = b"--b1\r\nContent-Type: application/dicom\r\n\r\nFIRST\r\n--b1\r\nContent-Type: application/dicom\r\n\r\nSECOND\r\n--b1--\r\n";
        assert_eq!(
            multipart_parts(body, "b1").unwrap(),
            [&b"FIRST"[..], &b"SECOND"[..]]
        );
        assert!(multipart_parts(b"no boundary", "b1").is_err());
        assert!(multipart_parts(b"--b1\r\n\r\nX\r\n--b1\r\n", "b1").is_err());
        assert_eq!(
            media_parameter(
                "multipart/related; type=\"application/dicom\"; boundary=\"abc\"",
                "boundary"
            ),
            Some("abc")
        );
    }
}
