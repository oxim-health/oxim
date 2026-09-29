//! SNMPv2c trap encoding (RFC 3416 PDUs in BER, RFC 1901 community
//! messages).

use crate::evaluate::Notification;

/// `sysUpTime.0`.
const SYS_UP_TIME: [u32; 9] = [1, 3, 6, 1, 2, 1, 1, 3, 0];
/// `snmpTrapOID.0`.
const SNMP_TRAP_OID: [u32; 11] = [1, 3, 6, 1, 6, 3, 1, 1, 4, 1, 0];

/// Parses a dotted OID.
pub(crate) fn parse_oid(text: &str) -> Result<Vec<u32>, String> {
    let arcs = text
        .trim()
        .trim_start_matches('.')
        .split('.')
        .map(|arc| {
            arc.parse::<u32>()
                .map_err(|_| format!("invalid OID {text:?}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if arcs.len() < 2 || arcs[0] > 2 || (arcs[0] < 2 && arcs[1] >= 40) {
        return Err(format!("invalid OID {text:?}"));
    }
    Ok(arcs)
}

fn length(out: &mut Vec<u8>, length: usize) {
    if length < 0x80 {
        out.push(length as u8);
    } else {
        let bytes = length.to_be_bytes();
        let skip = bytes.iter().take_while(|b| **b == 0).count();
        out.push(0x80 | (bytes.len() - skip) as u8);
        out.extend_from_slice(&bytes[skip..]);
    }
}

fn tlv(tag: u8, contents: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    length(&mut out, contents.len());
    out.extend_from_slice(contents);
    out
}

fn integer(tag: u8, value: i64) -> Vec<u8> {
    let bytes = value.to_be_bytes();
    let mut start = 0;
    while start < 7 {
        let (this, next) = (bytes[start], bytes[start + 1]);
        if (this == 0 && next & 0x80 == 0) || (this == 0xff && next & 0x80 != 0) {
            start += 1;
        } else {
            break;
        }
    }
    tlv(tag, &bytes[start..])
}

fn unsigned(tag: u8, value: u32) -> Vec<u8> {
    let mut bytes = value.to_be_bytes().to_vec();
    while bytes.len() > 1 && bytes[0] == 0 && bytes[1] & 0x80 == 0 {
        bytes.remove(0);
    }
    if bytes[0] & 0x80 != 0 {
        bytes.insert(0, 0);
    }
    tlv(tag, &bytes)
}

fn oid(arcs: &[u32]) -> Vec<u8> {
    let mut contents = Vec::new();
    let mut push = |mut value: u32| {
        let mut chunk = vec![(value & 0x7f) as u8];
        value >>= 7;
        while value > 0 {
            chunk.push(0x80 | (value & 0x7f) as u8);
            value >>= 7;
        }
        chunk.reverse();
        contents.extend(chunk);
    };
    push(arcs[0] * 40 + arcs.get(1).copied().unwrap_or(0));
    for arc in arcs.iter().skip(2) {
        push(*arc);
    }
    tlv(0x06, &contents)
}

fn varbind(name: &[u32], value: Vec<u8>) -> Vec<u8> {
    let mut contents = oid(name);
    contents.extend(value);
    tlv(0x30, &contents)
}

/// An SNMPv2c trap for `notification`. The alert fields are sent under the
/// trap OID: `.1` rule, `.2` subject, `.3` state, `.4` severity, `.5`
/// summary.
pub(crate) fn trap(
    community: &str,
    trap_oid: &[u32],
    notification: &Notification,
    request: u64,
) -> Vec<u8> {
    let field = |n: u32| {
        let mut name = trap_oid.to_vec();
        name.push(n);
        name
    };
    let text = |value: &str| tlv(0x04, value.as_bytes());
    // Hundredths of a second since the alert started, as TimeTicks.
    let ticks = u32::try_from(
        notification
            .at
            .unix_millis()
            .saturating_sub(notification.since.unix_millis())
            .max(0)
            / 10,
    )
    .unwrap_or(u32::MAX);
    let mut bindings = Vec::new();
    bindings.extend(varbind(&SYS_UP_TIME, unsigned(0x43, ticks)));
    bindings.extend(varbind(&SNMP_TRAP_OID, oid(trap_oid)));
    bindings.extend(varbind(&field(1), text(&notification.rule)));
    bindings.extend(varbind(&field(2), text(&notification.subject)));
    bindings.extend(varbind(&field(3), text(notification.state.as_str())));
    bindings.extend(varbind(&field(4), text(notification.severity.as_str())));
    bindings.extend(varbind(&field(5), text(&notification.summary)));
    let mut pdu = integer(0x02, i64::try_from(request & 0x7fff_ffff).unwrap_or(0));
    pdu.extend(integer(0x02, 0));
    pdu.extend(integer(0x02, 0));
    pdu.extend(tlv(0x30, &bindings));
    let mut message = integer(0x02, 1);
    message.extend(tlv(0x04, community.as_bytes()));
    message.extend(tlv(0xa7, &pdu));
    tlv(0x30, &message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_ber_values() {
        assert_eq!(
            oid(&[1, 3, 6, 1, 2, 1, 1, 3, 0]),
            [0x06, 0x08, 0x2b, 6, 1, 2, 1, 1, 3, 0]
        );
        assert_eq!(
            oid(&[1, 3, 6, 1, 4, 1, 311]),
            [0x06, 0x07, 0x2b, 6, 1, 4, 1, 0x82, 0x37]
        );
        assert_eq!(integer(0x02, 0), [0x02, 0x01, 0x00]);
        assert_eq!(integer(0x02, 128), [0x02, 0x02, 0x00, 0x80]);
        assert_eq!(integer(0x02, -1), [0x02, 0x01, 0xff]);
        assert_eq!(unsigned(0x43, 200), [0x43, 0x02, 0x00, 0xc8]);
        let long = tlv(0x04, &[0u8; 200]);
        assert_eq!(&long[..3], &[0x04, 0x81, 200]);
        assert!(parse_oid("1.3.6.1.3.8469.1").is_ok());
        assert!(parse_oid("1.x").is_err());
        assert!(parse_oid("3.1").is_err());
    }
}
