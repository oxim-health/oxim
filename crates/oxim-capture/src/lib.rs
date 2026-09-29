//! Capture files for OXIM: record what a device and its host exchange, and
//! replay the device side later.
//!
//! Connecting a real analyzer is easier with evidence: a recording of the
//! device's actual conversation can be replayed against OXIM as often as
//! needed, attached to a device profile as a fixture, and shared after
//! [`oxim-anonymize`](../oxim_anonymize/index.html) has removed protected
//! health information.
//!
//! - [`format`](mod@format): the `.oximcap` file format (JSON Lines) with a reader and
//!   a writer.
//! - [`proxy`]: a transparent TCP proxy and a serial line recorder that
//!   write captures.
//! - [`replay`](mod@replay): replays the device side of a capture against a host.
//!
//! Only connections explicitly routed through the proxy (or serial lines
//! bridged through it) are recorded. Passive, network-wide capture is the
//! job of the companion netKit project.

pub(crate) mod base64;
pub mod format;
pub mod proxy;
pub mod replay;

pub use format::{
    Capture, CaptureError, CaptureWriter, Direction, Event, FORMAT_NAME, FORMAT_VERSION, Header,
    Protocol, Record, Transport,
};
pub use proxy::{CaptureSink, DeviceSide, SerialLine, TcpProxy, now, record_serial};
pub use replay::{Endpoint, ReplayOptions, ReplayReport, replay, replay_listening};

/// Renders bytes for people: printable ASCII as is, line breaks and
/// protocol control characters by name (`<STX>`, `<ETX>`, `<CR>`...), and
/// other bytes as hexadecimal.
pub fn describe(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for &byte in bytes {
        let name = match byte {
            0x02 => "<STX>",
            0x03 => "<ETX>",
            0x04 => "<EOT>",
            0x05 => "<ENQ>",
            0x06 => "<ACK>",
            0x0A => "<LF>",
            0x0B => "<VT>",
            0x0D => "<CR>",
            0x15 => "<NAK>",
            0x17 => "<ETB>",
            0x1C => "<FS>",
            0x20..=0x7E => {
                out.push(char::from(byte));
                continue;
            }
            other => {
                out.push_str(&format!("<{other:02X}>"));
                continue;
            }
        };
        out.push_str(name);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_control_characters() {
        assert_eq!(
            describe(b"\x021H|\\^&\r\x03A0\r\n\x06\xff"),
            "<STX>1H|\\^&<CR><ETX>A0<CR><LF><ACK><FF>"
        );
    }
}
