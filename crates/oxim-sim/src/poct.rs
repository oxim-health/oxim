//! A simulated point-of-care device speaking POCT1-A to a data manager.
//!
//! The device follows the usual conversation: HEL, DST, observations, EOT
//! and END, waiting for an acknowledgment after each message and printing
//! whatever the host sends.

use std::io;
use std::time::Duration;

use oxim_poct1a::{SplitEvent, Splitter};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

fn header(control_id: u32) -> String {
    format!(
        "<HDR><HDR.control_id V=\"{control_id}\"/><HDR.version_id V=\"POCT1\"/>\
         <HDR.creation_dttm V=\"2026-09-29T12:00:00+03:00\"/></HDR>"
    )
}

/// The messages of one conversation, in order.
pub fn conversation(observations: usize) -> Vec<String> {
    let mut control = 0u32;
    let mut next = || {
        control += 1;
        header(control)
    };
    let mut messages = vec![
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><HEL.R01>{}<DEV><DEV.device_id V=\"SIM-POCT-1\"/>\
             <DEV.vendor_id V=\"OXIM\"/><DEV.model_id V=\"Simulator\"/><DEV.serial_id V=\"0001\"/>\
             <DEV.sw_version V=\"1.0\"/><DSC><DSC.connection_profile_cd V=\"SA\"/>\
             <DSC.topics_supported_cd V=\"OBS\"/></DSC></DEV></HEL.R01>",
            next()
        ),
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><DST.R01>{}<DST><DST.status_dttm V=\"2026-09-29T12:00:00+03:00\"/>\
             <DST.new_observations_qty V=\"{observations}\"/></DST></DST.R01>",
            next()
        ),
    ];
    for index in 0..observations {
        let value = format!("{:.2}", 7.35 + (index % 10) as f64 * 0.01);
        messages.push(format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><OBS.R01>{}<SVC><SVC.role_cd V=\"OBS\"/>\
             <SVC.observation_dttm V=\"2026-09-29T11:59:00+03:00\"/><PT><PT.patient_id V=\"SIM{index:04}\"/>\
             <OBS><OBS.observation_id V=\"pH\" SN=\"LOCAL\"/><OBS.value V=\"{value}\" U=\"\"/></OBS></PT>\
             <OPR><OPR.operator_id V=\"sim-operator\"/></OPR></SVC></OBS.R01>",
            next()
        ));
    }
    messages.push(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><EOT.R01>{}<EOT><EOT.topic_cd V=\"OBS\"/></EOT></EOT.R01>",
        next()
    ));
    messages.push(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><END.R01>{}<TRM><TRM.reason_cd V=\"NRM\"/></TRM></END.R01>",
        next()
    ));
    messages
}

/// Runs one device conversation against `target`, calling `on_host` for
/// every document the host sends. Returns the number of documents received.
pub async fn send(
    target: &str,
    observations: usize,
    reply_timeout: Duration,
    mut on_host: impl FnMut(&[u8]),
) -> io::Result<usize> {
    let mut stream = TcpStream::connect(target).await?;
    let mut splitter = Splitter::default();
    let mut received = 0;
    let mut buffer = [0u8; 8192];
    for message in conversation(observations) {
        stream.write_all(message.as_bytes()).await?;
        // Wait for the host's answer (an ACK, a request or a directive).
        let deadline = tokio::time::Instant::now() + reply_timeout;
        let mut answered = false;
        while !answered {
            while let Some(event) = splitter.next_event() {
                if let SplitEvent::Document(document) = event {
                    received += 1;
                    on_host(&document);
                    answered = true;
                }
            }
            if answered {
                break;
            }
            match tokio::time::timeout_at(deadline, stream.read(&mut buffer)).await {
                Ok(Ok(0)) => return Ok(received),
                Ok(Ok(n)) => splitter.push(&buffer[..n]),
                Ok(Err(e)) => return Err(e),
                Err(_) => break,
            }
        }
    }
    Ok(received)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversation_messages_parse() {
        for message in conversation(3) {
            oxim_poct1a::Message::parse(message.as_bytes()).unwrap();
        }
    }
}
