//! What the rules look at: one reading of queues, errors, devices, disks
//! and certificates.

use std::path::{Path, PathBuf};

use oxim_devices::DeviceState;
use oxim_model::{ChannelId, ClinicalDateTime, ConnectorId, Timestamp};
use oxim_store::QueueStats;
use rustls_pki_types::CertificateDer;
use rustls_pki_types::pem::PemObject;

/// The queue of one destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueSample {
    /// The channel.
    pub channel: ChannelId,
    /// The destination.
    pub destination: ConnectorId,
    /// Its counts.
    pub stats: QueueStats,
}

/// Messages that ended in error within a rule's window, per channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorSample {
    /// The rule the count was taken for.
    pub rule: String,
    /// The channel.
    pub channel: ChannelId,
    /// Errored messages received within the window (capped at the rule's
    /// threshold plus one).
    pub errors: u64,
}

/// The free space of one volume.
#[derive(Debug, Clone, PartialEq)]
pub struct DiskSample {
    /// The rule the reading was taken for.
    pub rule: String,
    /// A directory on the volume.
    pub path: PathBuf,
    /// Bytes available to OXIM.
    pub available: u64,
    /// Size of the volume.
    pub total: u64,
}

/// The expiry of one certificate file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertificateSample {
    /// The rule the reading was taken for.
    pub rule: String,
    /// The file.
    pub path: PathBuf,
    /// The earliest `notAfter` of the certificates in the file, or why it
    /// could not be read.
    pub not_after: Result<Timestamp, String>,
}

/// One reading of everything the rules look at.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    /// When the reading was taken.
    pub now: Option<Timestamp>,
    /// Destination queues of the deployed channels.
    pub queues: Vec<QueueSample>,
    /// Errored messages per rule and channel.
    pub errors: Vec<ErrorSample>,
    /// Devices past their silence window.
    pub silent_devices: Vec<DeviceState>,
    /// Watched volumes.
    pub disks: Vec<DiskSample>,
    /// Watched certificates.
    pub certificates: Vec<CertificateSample>,
}

/// Reads the free space of the volume holding `path` for `rule`.
pub fn disk(rule: &str, path: &Path) -> std::io::Result<DiskSample> {
    Ok(DiskSample {
        rule: rule.to_owned(),
        path: path.to_owned(),
        available: fs4::available_space(path)?,
        total: fs4::total_space(path)?,
    })
}

/// Reads the earliest expiry of the PEM certificates in `path` for `rule`.
pub fn certificate(rule: &str, path: &Path) -> CertificateSample {
    let not_after = (|| {
        let certificates = CertificateDer::pem_file_iter(path)
            .and_then(|items| items.collect::<Result<Vec<_>, _>>())
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        certificates
            .iter()
            .map(|certificate| not_after(certificate.as_ref()))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .min()
            .ok_or_else(|| format!("{} contains no certificate", path.display()))
    })();
    CertificateSample {
        rule: rule.to_owned(),
        path: path.to_owned(),
        not_after,
    }
}

/// One DER element: its tag and contents, and the rest of the input.
fn element(input: &[u8]) -> Result<(u8, &[u8], &[u8]), String> {
    let invalid = || "invalid DER".to_owned();
    let (&tag, rest) = input.split_first().ok_or_else(invalid)?;
    let (&first, mut rest) = rest.split_first().ok_or_else(invalid)?;
    let length = if first < 0x80 {
        usize::from(first)
    } else {
        let count = usize::from(first & 0x7f);
        if count == 0 || count > 4 || rest.len() < count {
            return Err(invalid());
        }
        let (bytes, tail) = rest.split_at(count);
        rest = tail;
        bytes
            .iter()
            .fold(0usize, |length, byte| (length << 8) | usize::from(*byte))
    };
    if rest.len() < length {
        return Err(invalid());
    }
    let (contents, tail) = rest.split_at(length);
    Ok((tag, contents, tail))
}

/// The `notAfter` time of an X.509 certificate.
pub fn not_after(der: &[u8]) -> Result<Timestamp, String> {
    const SEQUENCE: u8 = 0x30;
    let (tag, certificate, _) = element(der)?;
    if tag != SEQUENCE {
        return Err("not a certificate".into());
    }
    let (tag, tbs, _) = element(certificate)?;
    if tag != SEQUENCE {
        return Err("not a certificate".into());
    }
    let mut rest = tbs;
    let (tag, _, after) = element(rest)?;
    // The optional explicit version.
    if tag == 0xa0 {
        rest = after;
    }
    // Serial number, signature algorithm, issuer.
    for _ in 0..3 {
        rest = element(rest)?.2;
    }
    let (tag, validity, _) = element(rest)?;
    if tag != SEQUENCE {
        return Err("certificate without validity".into());
    }
    let (_, _, rest) = element(validity)?;
    let (tag, time, _) = element(rest)?;
    let text = std::str::from_utf8(time).map_err(|_| "invalid certificate time".to_owned())?;
    parse_time(tag, text)
}

fn parse_time(tag: u8, text: &str) -> Result<Timestamp, String> {
    let invalid = || format!("invalid certificate time {text:?}");
    let text = text.strip_suffix('Z').ok_or_else(invalid)?;
    let (year, rest) = match tag {
        // UTCTime: two-digit years, 1950 to 2049 (RFC 5280).
        0x17 => {
            let (yy, rest) = text.split_at_checked(2).ok_or_else(invalid)?;
            let yy: u16 = yy.parse().map_err(|_| invalid())?;
            (if yy >= 50 { 1900 + yy } else { 2000 + yy }, rest)
        }
        // GeneralizedTime.
        0x18 => {
            let (yyyy, rest) = text.split_at_checked(4).ok_or_else(invalid)?;
            (yyyy.parse().map_err(|_| invalid())?, rest)
        }
        _ => return Err(invalid()),
    };
    if rest.len() != 10 || !rest.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid());
    }
    let part = |i: usize| rest[i..i + 2].parse::<u8>().map_err(|_| invalid());
    ClinicalDateTime::date_time(
        (year, part(0)?, part(2)?),
        (part(4)?, part(6)?, part(8)?),
        Some(0),
    )
    .ok()
    .and_then(|time| time.to_timestamp(0))
    .ok_or_else(invalid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_certificate_expiry() {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(vec!["localhost".to_owned()]).unwrap();
        params.not_after = rcgen::date_time_ymd(2031, 3, 14);
        let certificate = params.self_signed(&key).unwrap();
        let expiry = not_after(certificate.der()).unwrap();
        assert_eq!(expiry, "2031-03-14T00:00:00Z".parse().unwrap());

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cert.pem");
        std::fs::write(&path, certificate.pem()).unwrap();
        assert_eq!(super::certificate("certs", &path).not_after, Ok(expiry));
        assert!(
            super::certificate("certs", &dir.path().join("missing.pem"))
                .not_after
                .is_err()
        );
        assert!(not_after(b"\x30\x03\x02\x01").is_err());
    }

    #[test]
    fn parses_both_time_forms() {
        assert_eq!(
            parse_time(0x17, "491231235959Z").unwrap(),
            "2049-12-31T23:59:59Z".parse().unwrap()
        );
        assert_eq!(
            parse_time(0x17, "500101000000Z").unwrap(),
            "1950-01-01T00:00:00Z".parse().unwrap()
        );
        assert_eq!(
            parse_time(0x18, "20310314120000Z").unwrap(),
            "2031-03-14T12:00:00Z".parse().unwrap()
        );
        assert!(parse_time(0x17, "4912312359Z").is_err());
    }

    #[test]
    fn measures_disks() {
        let dir = tempfile::tempdir().unwrap();
        let sample = disk("disk", dir.path()).unwrap();
        assert!(sample.total >= sample.available);
        assert!(sample.total > 0);
    }
}
