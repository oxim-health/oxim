//! AWS Signature Version 4 for requests with the signature in the
//! `Authorization` header, as S3-compatible object stores require.
//!
//! The payload is always hashed (`x-amz-content-sha256`), never sent as
//! `UNSIGNED-PAYLOAD`.

use ring::{digest, hmac};

/// Access keys.
#[derive(Clone, PartialEq, Eq)]
pub struct Credentials {
    /// Access key id.
    pub access_key_id: String,
    /// Secret access key.
    pub secret_access_key: String,
    /// Session token of temporary credentials.
    pub session_token: Option<String>,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("access_key_id", &self.access_key_id)
            .finish_non_exhaustive()
    }
}

/// A request to sign.
#[derive(Debug, Clone, Copy)]
pub struct SignedRequest<'a> {
    /// Method, such as `GET`.
    pub method: &'a str,
    /// The path as sent, already URI-encoded (see [`uri_encode`]).
    pub path: &'a str,
    /// Query parameters, not encoded.
    pub query: &'a [(String, String)],
    /// Headers to sign (names in any case), including `host` and
    /// `x-amz-date`.
    pub headers: &'a [(String, String)],
    /// Hex SHA-256 of the payload.
    pub payload_hash: &'a str,
}

/// The scope of a signature.
#[derive(Debug, Clone, Copy)]
pub struct Scope<'a> {
    /// Region, such as `eu-central-1`.
    pub region: &'a str,
    /// Service, `s3` for object storage.
    pub service: &'a str,
    /// Request time as `YYYYMMDDTHHMMSSZ`, the `x-amz-date` value.
    pub amz_date: &'a str,
}

/// Hex-encoded SHA-256.
pub fn sha256_hex(data: &[u8]) -> String {
    hex(digest::digest(&digest::SHA256, data).as_ref())
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 15)]));
    }
    out
}

/// URI encoding as SigV4 defines it: unreserved characters (`A-Z a-z 0-9
/// - _ . ~`) stay, everything else becomes `%XX`; `/` stays unless
/// `encode_slash`.
pub fn uri_encode(input: &str, encode_slash: bool) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(char::from(byte));
            }
            b'/' if !encode_slash => out.push('/'),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// The canonical query string: encoded pairs sorted by key, then value.
pub fn canonical_query(query: &[(String, String)]) -> String {
    let mut pairs: Vec<(String, String)> = query
        .iter()
        .map(|(key, value)| (uri_encode(key, true), uri_encode(value, true)))
        .collect();
    pairs.sort();
    pairs
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("&")
}

fn canonical_headers(headers: &[(String, String)]) -> (String, String) {
    let mut normalized: Vec<(String, String)> = headers
        .iter()
        .map(|(name, value)| {
            (
                name.to_ascii_lowercase(),
                value.split_whitespace().collect::<Vec<_>>().join(" "),
            )
        })
        .collect();
    normalized.sort_by(|a, b| a.0.cmp(&b.0));
    let mut merged: Vec<(String, String)> = Vec::with_capacity(normalized.len());
    for (name, value) in normalized {
        match merged.last_mut() {
            Some((last, values)) if *last == name => {
                values.push(',');
                values.push_str(&value);
            }
            _ => merged.push((name, value)),
        }
    }
    let block = merged
        .iter()
        .map(|(name, value)| format!("{name}:{value}\n"))
        .collect::<String>();
    let signed = merged
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>()
        .join(";");
    (block, signed)
}

/// The canonical request and the signed header list.
pub fn canonical_request(request: &SignedRequest<'_>) -> (String, String) {
    let (headers, signed) = canonical_headers(request.headers);
    let canonical = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        request.method,
        request.path,
        canonical_query(request.query),
        headers,
        signed,
        request.payload_hash
    );
    (canonical, signed)
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, key), data)
        .as_ref()
        .to_vec()
}

/// The signature and the signed header list.
pub fn signature(
    request: &SignedRequest<'_>,
    credentials: &Credentials,
    scope: &Scope<'_>,
) -> (String, String) {
    let date = scope.amz_date.get(..8).unwrap_or(scope.amz_date);
    let (canonical, signed) = canonical_request(request);
    let credential_scope = format!("{date}/{}/{}/aws4_request", scope.region, scope.service);
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{}\n{credential_scope}\n{}",
        scope.amz_date,
        sha256_hex(canonical.as_bytes())
    );
    let key = hmac_sha256(
        format!("AWS4{}", credentials.secret_access_key).as_bytes(),
        date.as_bytes(),
    );
    let key = hmac_sha256(&key, scope.region.as_bytes());
    let key = hmac_sha256(&key, scope.service.as_bytes());
    let key = hmac_sha256(&key, b"aws4_request");
    (hex(&hmac_sha256(&key, string_to_sign.as_bytes())), signed)
}

/// The `Authorization` header value.
pub fn authorization(
    request: &SignedRequest<'_>,
    credentials: &Credentials,
    scope: &Scope<'_>,
) -> String {
    let date = scope.amz_date.get(..8).unwrap_or(scope.amz_date);
    let (signature, signed) = signature(request, credentials, scope);
    format!(
        "AWS4-HMAC-SHA256 Credential={}/{date}/{}/{}/aws4_request, SignedHeaders={signed}, Signature={signature}",
        credentials.access_key_id, scope.region, scope.service
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const EMPTY: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
        items
            .iter()
            .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
            .collect()
    }

    /// `get-vanilla` of the AWS Signature Version 4 test suite.
    #[test]
    fn signs_the_vanilla_test_vector() {
        let credentials = Credentials {
            access_key_id: "AKIDEXAMPLE".into(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".into(),
            session_token: None,
        };
        let headers = pairs(&[
            ("Host", "example.amazonaws.com"),
            ("X-Amz-Date", "20150830T123600Z"),
        ]);
        let request = SignedRequest {
            method: "GET",
            path: "/",
            query: &[],
            headers: &headers,
            payload_hash: EMPTY,
        };
        let scope = Scope {
            region: "us-east-1",
            service: "service",
            amz_date: "20150830T123600Z",
        };
        assert_eq!(
            authorization(&request, &credentials, &scope),
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, \
             SignedHeaders=host;x-amz-date, \
             Signature=5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31"
        );
    }

    fn s3_credentials() -> Credentials {
        Credentials {
            access_key_id: "AKIAIOSFODNN7EXAMPLE".into(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into(),
            session_token: None,
        }
    }

    const S3_SCOPE: Scope<'static> = Scope {
        region: "us-east-1",
        service: "s3",
        amz_date: "20130524T000000Z",
    };

    /// The GET Object example of the Amazon S3 SigV4 documentation.
    #[test]
    fn signs_the_s3_get_object_example() {
        let headers = pairs(&[
            ("Host", "examplebucket.s3.amazonaws.com"),
            ("Range", "bytes=0-9"),
            ("x-amz-content-sha256", EMPTY),
            ("x-amz-date", "20130524T000000Z"),
        ]);
        let request = SignedRequest {
            method: "GET",
            path: "/test.txt",
            query: &[],
            headers: &headers,
            payload_hash: EMPTY,
        };
        let (signature, signed) = signature(&request, &s3_credentials(), &S3_SCOPE);
        assert_eq!(signed, "host;range;x-amz-content-sha256;x-amz-date");
        assert_eq!(
            signature,
            "f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
        );
    }

    /// The PUT Object example of the Amazon S3 SigV4 documentation.
    #[test]
    fn signs_the_s3_put_object_example() {
        let payload_hash = sha256_hex(b"Welcome to Amazon S3.");
        assert_eq!(
            payload_hash,
            "44ce7dd67c959e0d3524ffac1771dfbba87d2b6b4b4e99e42034a8b803f8b072"
        );
        let headers = pairs(&[
            ("Date", "Fri, 24 May 2013 00:00:00 GMT"),
            ("Host", "examplebucket.s3.amazonaws.com"),
            ("x-amz-date", "20130524T000000Z"),
            ("x-amz-storage-class", "REDUCED_REDUNDANCY"),
            ("x-amz-content-sha256", &payload_hash),
        ]);
        let path = format!("/{}", uri_encode("test$file.text", false));
        let request = SignedRequest {
            method: "PUT",
            path: &path,
            query: &[],
            headers: &headers,
            payload_hash: &payload_hash,
        };
        let (signature, signed) = signature(&request, &s3_credentials(), &S3_SCOPE);
        assert_eq!(
            signed,
            "date;host;x-amz-content-sha256;x-amz-date;x-amz-storage-class"
        );
        assert_eq!(
            signature,
            "98ad721746da40c64f1a55b78f14c238d841ea1380cd77a1b5971af0ece108bd"
        );
    }

    /// The GET Bucket (List Objects) example of the Amazon S3 SigV4
    /// documentation.
    #[test]
    fn signs_the_s3_list_objects_example() {
        let headers = pairs(&[
            ("Host", "examplebucket.s3.amazonaws.com"),
            ("x-amz-content-sha256", EMPTY),
            ("x-amz-date", "20130524T000000Z"),
        ]);
        let query = pairs(&[("max-keys", "2"), ("prefix", "J")]);
        let request = SignedRequest {
            method: "GET",
            path: "/",
            query: &query,
            headers: &headers,
            payload_hash: EMPTY,
        };
        let (signature, _) = signature(&request, &s3_credentials(), &S3_SCOPE);
        assert_eq!(
            signature,
            "34b48302e7b5fa45bde8084f4b7868a86f0a534bc59db6670ed5711ef69dc6f7"
        );
    }

    #[test]
    fn encodes_uris() {
        assert_eq!(uri_encode("in/a b+c~.hl7", false), "in/a%20b%2Bc~.hl7");
        assert_eq!(uri_encode("in/x", true), "in%2Fx");
        assert_eq!(
            canonical_query(&pairs(&[
                ("prefix", "in/"),
                ("delimiter", "/"),
                ("list-type", "2")
            ])),
            "delimiter=%2F&list-type=2&prefix=in%2F"
        );
    }
}
