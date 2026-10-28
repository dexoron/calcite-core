//! Minecraft protocol primitives for the protocol-775 migration target.

use bytes::{BufMut, BytesMut};

pub const PROTOCOL_VERSION: i32 = 775;
pub const MINECRAFT_VERSION: &str = "26.1.2";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ProtocolError {
    #[error("VarInt exceeds five bytes")]
    VarIntTooBig,
    #[error("packet length exceeds configured maximum")]
    PacketTooLarge,
    #[error("packet length cannot be negative")]
    NegativePacketLength,
    #[error("packet is truncated")]
    Truncated,
    #[error("packet contains unexpected trailing data")]
    TrailingData,
    #[error("unexpected packet id {0}")]
    UnexpectedPacketId(i32),
    #[error("unsupported protocol version {0}")]
    UnsupportedProtocolVersion(i32),
    #[error("invalid handshake next state {0}")]
    InvalidNextState(i32),
    #[error("server address is too long")]
    ServerAddressTooLong,
    #[error("server address is not valid UTF-8")]
    InvalidServerAddress,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandshakeIntent {
    Status,
    Login,
    Transfer,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handshake {
    pub protocol_version: i32,
    pub server_address: String,
    pub server_port: u16,
    pub intent: HandshakeIntent,
}

/// Decode the uncompressed handshake payload (packet id through next-state).
/// Protocol 775 is intentionally enforced so packet layouts from other
/// Minecraft versions cannot silently enter the state machine.
pub fn parse_handshake(mut payload: &[u8]) -> Result<Handshake, ProtocolError> {
    let packet_id = read_var_i32(&mut payload)?;
    if packet_id != 0 {
        return Err(ProtocolError::UnexpectedPacketId(packet_id));
    }
    let protocol_version = read_var_i32(&mut payload)?;
    if protocol_version != PROTOCOL_VERSION {
        return Err(ProtocolError::UnsupportedProtocolVersion(protocol_version));
    }
    let address_len = read_var_i32(&mut payload)?;
    if !(0..=255).contains(&address_len) {
        return Err(ProtocolError::ServerAddressTooLong);
    }
    let address_len = address_len as usize;
    if payload.len() < address_len {
        return Err(ProtocolError::Truncated);
    }
    let server_address = std::str::from_utf8(&payload[..address_len])
        .map_err(|_| ProtocolError::InvalidServerAddress)?
        .to_owned();
    payload = &payload[address_len..];
    if payload.len() < 2 {
        return Err(ProtocolError::Truncated);
    }
    let server_port = u16::from_be_bytes([payload[0], payload[1]]);
    payload = &payload[2..];
    let intent = match read_var_i32(&mut payload)? {
        1 => HandshakeIntent::Status,
        2 => HandshakeIntent::Login,
        3 => HandshakeIntent::Transfer,
        value => return Err(ProtocolError::InvalidNextState(value)),
    };
    if !payload.is_empty() {
        return Err(ProtocolError::TrailingData);
    }
    Ok(Handshake {
        protocol_version,
        server_address,
        server_port,
        intent,
    })
}

pub fn read_var_i32(buf: &mut &[u8]) -> Result<i32, ProtocolError> {
    let mut result = 0u32;
    for shift in (0..35).step_by(7) {
        let byte = *buf.first().ok_or(ProtocolError::Truncated)?;
        *buf = &buf[1..];
        if shift == 28 && byte & 0x70 != 0 {
            return Err(ProtocolError::VarIntTooBig);
        }
        result |= u32::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(result as i32);
        }
    }
    Err(ProtocolError::VarIntTooBig)
}

pub fn write_var_i32(mut value: i32, out: &mut Vec<u8>) {
    loop {
        let mut byte = (value as u8) & 0x7f;
        value = ((value as u32) >> 7) as i32;
        if value != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if value == 0 {
            return;
        }
    }
}

pub fn encode_frame(payload: &[u8]) -> Result<BytesMut, ProtocolError> {
    let mut length = Vec::new();
    write_var_i32(
        i32::try_from(payload.len()).map_err(|_| ProtocolError::PacketTooLarge)?,
        &mut length,
    );
    if length.len() > 3 {
        return Err(ProtocolError::PacketTooLarge);
    }
    let mut frame = BytesMut::with_capacity(length.len() + payload.len());
    frame.put_slice(&length);
    frame.put_slice(payload);
    Ok(frame)
}

pub fn decode_frame(buf: &mut BytesMut) -> Result<Option<BytesMut>, ProtocolError> {
    let mut view = &buf[..];
    let length = match read_var_i32(&mut view) {
        Ok(value) if value >= 0 => value as usize,
        Ok(_) => return Err(ProtocolError::NegativePacketLength),
        Err(ProtocolError::Truncated) => return Ok(None),
        Err(error) => return Err(error),
    };
    if view.len() < length {
        return Ok(None);
    }
    let header_len = buf.len() - view.len();
    let frame = buf.split_to(header_len + length).split_off(header_len);
    Ok(Some(frame))
}

pub fn write_string(value: &str, out: &mut Vec<u8>) -> Result<(), ProtocolError> {
    let len = i32::try_from(value.len()).map_err(|_| ProtocolError::PacketTooLarge)?;
    write_var_i32(len, out);
    out.extend_from_slice(value.as_bytes());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_boundaries_round_trip() {
        for value in [0, 1, 127, 128, 255, 2097151, i32::MAX, -1] {
            let mut encoded = Vec::new();
            write_var_i32(value, &mut encoded);
            let mut input = encoded.as_slice();
            assert_eq!(read_var_i32(&mut input), Ok(value));
            assert!(input.is_empty());
        }
    }

    #[test]
    fn frame_waits_for_partial_payload() {
        let frame = encode_frame(&[1, 2, 3]).unwrap();
        let mut input = BytesMut::from(&frame[..2]);
        assert_eq!(decode_frame(&mut input).unwrap(), None);
        input.extend_from_slice(&frame[2..]);
        assert_eq!(
            decode_frame(&mut input).unwrap().unwrap(),
            &b"\x01\x02\x03"[..]
        );
    }

    #[test]
    fn negative_frame_length_is_rejected() {
        let mut input = BytesMut::from(&[0xff, 0xff, 0xff, 0xff, 0x0f][..]);
        assert_eq!(
            decode_frame(&mut input),
            Err(ProtocolError::NegativePacketLength)
        );
    }

    #[test]
    fn rejects_varint_with_noncanonical_fifth_byte() {
        let mut input = &[0xff, 0xff, 0xff, 0xff, 0x10][..];
        assert_eq!(read_var_i32(&mut input), Err(ProtocolError::VarIntTooBig));
    }

    #[test]
    fn rejects_invalid_utf8_handshake_address() {
        let mut payload = Vec::new();
        write_var_i32(0, &mut payload);
        write_var_i32(PROTOCOL_VERSION, &mut payload);
        write_var_i32(1, &mut payload);
        payload.push(0xff);
        payload.extend_from_slice(&25565u16.to_be_bytes());
        write_var_i32(1, &mut payload);

        assert_eq!(
            parse_handshake(&payload),
            Err(ProtocolError::InvalidServerAddress)
        );
    }

    fn handshake_bytes(version: i32, intent: i32) -> Vec<u8> {
        let mut out = Vec::new();
        write_var_i32(0, &mut out);
        write_var_i32(version, &mut out);
        write_string("localhost", &mut out).unwrap();
        out.extend_from_slice(&25565u16.to_be_bytes());
        write_var_i32(intent, &mut out);
        out
    }

    #[test]
    fn parses_protocol_775_handshake() {
        let parsed = parse_handshake(&handshake_bytes(PROTOCOL_VERSION, 2)).unwrap();
        assert_eq!(parsed.server_address, "localhost");
        assert_eq!(parsed.server_port, 25565);
        assert_eq!(parsed.intent, HandshakeIntent::Login);
    }

    #[test]
    fn rejects_handshake_with_wrong_packet_id() {
        let mut payload = handshake_bytes(PROTOCOL_VERSION, 1);
        payload[0] = 1;
        assert_eq!(
            parse_handshake(&payload),
            Err(ProtocolError::UnexpectedPacketId(1))
        );
    }

    #[test]
    fn rejects_wrong_version_and_intent() {
        assert_eq!(
            parse_handshake(&handshake_bytes(774, 1)),
            Err(ProtocolError::UnsupportedProtocolVersion(774))
        );
        assert_eq!(
            parse_handshake(&handshake_bytes(PROTOCOL_VERSION, 9)),
            Err(ProtocolError::InvalidNextState(9))
        );
    }

    #[test]
    fn rejects_truncated_and_trailing_handshake() {
        let payload = handshake_bytes(PROTOCOL_VERSION, 1);
        assert_eq!(
            parse_handshake(&payload[..payload.len() - 1]),
            Err(ProtocolError::Truncated)
        );
        let mut trailing = payload;
        trailing.push(0);
        assert_eq!(parse_handshake(&trailing), Err(ProtocolError::TrailingData));
    }
}
