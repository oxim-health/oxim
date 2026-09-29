//! Remote file, object storage, email and SOAP connectors for OXIM.
//!
//! | Type | Source | Destination | Module |
//! |---|---|---|---|
//! | `sftp` | Remote directory poller over SFTP | Atomic file writer over SFTP | [`sftp`] |
//! | `ftp` | Remote directory poller over FTP/FTPS | Atomic file writer over FTP/FTPS | [`ftp`] |
//! | `s3` | Object poller for S3-compatible storage | Object writer | [`s3`] |
//! | `smtp` | | Email with the message as body or attachment | [`smtp`] |
//! | `imap` | Emails (or their attachments) from a mailbox folder | | [`imap`] |
//! | `soap` | SOAP 1.1/1.2 endpoint | SOAP 1.1/1.2 client with WS-Security and OAuth 2.0 | [`soap`] |
//!
//! Sources acknowledge (delete, move or mark) remote items only after the
//! message is stored durably. Destinations report temporary failures
//! (retried) and permanent ones. Connector factories never open
//! connections, so validating channel files works offline; secrets named by
//! `*_env` settings are read when a connection is opened. TLS uses rustls
//! with the ring provider and the `tls` settings of
//! [`oxim_connectors::tls::ClientTlsSettings`].
//!
//! SMB/CIFS shares are not a connector type: no maintained pure-Rust SMB
//! client fits OXIM's dependency policy yet. Mount the share on the host
//! (`mount -t cifs`, or a mapped UNC path on Windows) and use the local
//! `file` connector.

mod remote;
mod util;
mod xml;

pub mod ftp;
pub mod imap;
pub mod s3;
pub mod sftp;
pub mod smtp;
pub mod soap;

/// Registers every connector of this crate.
pub fn register(registry: &mut oxim_core::Registry) {
    sftp::register(registry);
    ftp::register(registry);
    s3::register(registry);
    smtp::register(registry);
    imap::register(registry);
    soap::register(registry);
}
