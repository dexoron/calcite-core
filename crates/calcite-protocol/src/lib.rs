//! Minecraft protocol primitives for the protocol-775 migration target.

use bytes::{BufMut, BytesMut};
use md5::{Digest, Md5};

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
    #[error("invalid login packet id {0}")]
    InvalidLoginPacketId(i32),
    #[error("player name must contain between 1 and 16 bytes")]
    InvalidPlayerName,
    #[error("player name is not valid UTF-8")]
    InvalidPlayerNameEncoding,
    #[error("login UUID derivation is unavailable until a vetted crypto dependency is selected")]
    OfflineUuidUnavailable,
    #[error("login acknowledged packet is malformed")]
    InvalidLoginAcknowledged,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginStart {
    pub name: String,
    pub client_uuid: [u8; 16],
}

/// Parse the protocol-775 Login Start payload. Offline UUID derivation is
/// intentionally separate: the C++ oracle uses MD5, but this crate does not
/// yet carry a vetted crypto dependency, so no hand-rolled implementation is
/// provided.
pub fn parse_login_start(mut payload: &[u8]) -> Result<LoginStart, ProtocolError> {
    let packet_id = read_var_i32(&mut payload)?;
    if packet_id != 0 {
        return Err(ProtocolError::InvalidLoginPacketId(packet_id));
    }
    let name_len = read_var_i32(&mut payload)?;
    if !(1..=16).contains(&name_len) {
        return Err(ProtocolError::InvalidPlayerName);
    }
    let name_len = name_len as usize;
    if payload.len() < name_len + 16 {
        return Err(ProtocolError::Truncated);
    }
    let name = std::str::from_utf8(&payload[..name_len])
        .map_err(|_| ProtocolError::InvalidPlayerNameEncoding)?
        .to_owned();
    payload = &payload[name_len..];
    let mut client_uuid = [0u8; 16];
    client_uuid.copy_from_slice(&payload[..16]);
    payload = &payload[16..];
    if !payload.is_empty() {
        return Err(ProtocolError::TrailingData);
    }
    Ok(LoginStart { name, client_uuid })
}

pub fn derive_offline_uuid(_name: &str) -> Result<[u8; 16], ProtocolError> {
    let mut hasher = Md5::new();
    hasher.update(b"OfflinePlayer:");
    hasher.update(_name.as_bytes());
    let mut uuid: [u8; 16] = hasher.finalize().into();
    uuid[6] = (uuid[6] & 0x0f) | 0x30;
    uuid[8] = (uuid[8] & 0x3f) | 0x80;
    Ok(uuid)
}

pub fn encode_login_success(login: &LoginStart) -> Result<Vec<u8>, ProtocolError> {
    let uuid = derive_offline_uuid(&login.name)?;
    let mut payload = vec![0x02];
    payload.extend_from_slice(&uuid);
    write_string(&login.name, &mut payload)?;
    write_var_i32(0, &mut payload);
    Ok(payload)
}

pub fn parse_login_acknowledged(payload: &[u8]) -> Result<(), ProtocolError> {
    if payload == [0x03] {
        Ok(())
    } else {
        Err(ProtocolError::InvalidLoginAcknowledged)
    }
}

pub fn encode_configuration_finish() -> Vec<u8> {
    vec![0x03]
}

pub fn parse_configuration_acknowledged(payload: &[u8]) -> Result<(), ProtocolError> {
    if payload == [0x03] {
        Ok(())
    } else {
        Err(ProtocolError::InvalidLoginAcknowledged)
    }
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
    fn protocol_775_status_handshake_matches_wire_fixture() {
        let expected = [
            0x00, 0x87, 0x06, 0x09, b'l', b'o', b'c', b'a', b'l', b'h', b'o', b's', b't', 0x63,
            0xDD, 0x01,
        ];
        assert_eq!(handshake_bytes(PROTOCOL_VERSION, 1), expected);
        let parsed = parse_handshake(&expected).unwrap();
        assert_eq!(parsed.intent, HandshakeIntent::Status);
    }

    #[test]
    fn status_request_and_ping_frames_match_cxx_fixtures() {
        assert_eq!(encode_frame(&[0]).unwrap().as_ref(), &[0x01, 0x00]);
        let ping = [0x01, 1, 2, 3, 4, 5, 6, 7, 8];
        assert_eq!(
            encode_frame(&ping).unwrap().as_ref(),
            &[0x09, 0x01, 1, 2, 3, 4, 5, 6, 7, 8]
        );
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

    fn login_bytes(name: &[u8], uuid: [u8; 16]) -> Vec<u8> {
        let mut payload = Vec::new();
        write_var_i32(0, &mut payload);
        write_var_i32(name.len() as i32, &mut payload);
        payload.extend_from_slice(name);
        payload.extend_from_slice(&uuid);
        payload
    }

    #[test]
    fn parses_login_start() {
        let uuid = [7; 16];
        let parsed = parse_login_start(&login_bytes(b"Steve", uuid)).unwrap();
        assert_eq!(parsed.name, "Steve");
        assert_eq!(parsed.client_uuid, uuid);
    }

    #[test]
    fn login_start_wire_fixture_matches_cpp_layout() {
        let payload = login_bytes(b"Steve", [0; 16]);
        let mut frame = vec![0x17];
        frame.extend_from_slice(&payload);
        assert_eq!(
            &frame[..8],
            &[0x17, 0x00, 0x05, b'S', b't', b'e', b'v', b'e']
        );
        assert_eq!(frame.len(), 24);
        assert!(frame[8..].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn login_success_and_acknowledged_wire_fixtures_match_cpp_layout() {
        let offline_uuid = [
            0x56, 0x27, 0xdd, 0x98, 0xe6, 0xbe, 0x3c, 0x21, 0xb8, 0xa8, 0xe9, 0x23, 0x44, 0x18,
            0x36, 0x41,
        ];
        let mut success = vec![0x02];
        success.extend_from_slice(&offline_uuid);
        success.extend_from_slice(&[0x05, b'S', b't', b'e', b'v', b'e', 0x00]);
        assert_eq!(encode_frame(&success).unwrap().as_ref()[..2], [0x18, 0x02]);
        assert_eq!(encode_frame(&[0x03]).unwrap().as_ref(), &[0x01, 0x03]);
    }

    #[test]
    fn rejects_malformed_login_start() {
        assert_eq!(
            parse_login_start(&[0, 0]),
            Err(ProtocolError::InvalidPlayerName)
        );
        assert_eq!(
            parse_login_start(&[1]),
            Err(ProtocolError::InvalidLoginPacketId(1))
        );
        assert_eq!(
            parse_login_start(&login_bytes(b"Steve", [0; 16])[..20]),
            Err(ProtocolError::Truncated)
        );
        let mut trailing = login_bytes(b"Steve", [0; 16]);
        trailing.push(0);
        assert_eq!(
            parse_login_start(&trailing),
            Err(ProtocolError::TrailingData)
        );
    }

    #[test]
    fn rejects_login_name_with_invalid_utf8() {
        assert_eq!(
            parse_login_start(&login_bytes(&[0xff], [0; 16])),
            Err(ProtocolError::InvalidPlayerNameEncoding)
        );
    }

    #[test]
    fn offline_uuid_matches_cpp_vector() {
        assert_eq!(
            derive_offline_uuid("Steve").unwrap(),
            [
                0x56, 0x27, 0xdd, 0x98, 0xe6, 0xbe, 0x3c, 0x21, 0xb8, 0xa8, 0xe9, 0x23, 0x44, 0x18,
                0x36, 0x41
            ]
        );
    }

    #[test]
    fn login_success_and_acknowledged_are_encoded() {
        let login = LoginStart {
            name: "Steve".into(),
            client_uuid: [0; 16],
        };
        let success = encode_login_success(&login).unwrap();
        assert_eq!(success[0], 0x02);
        assert_eq!(&success[1..17], &derive_offline_uuid("Steve").unwrap());
        assert_eq!(parse_login_acknowledged(&[0x03]), Ok(()));
        assert_eq!(
            parse_login_acknowledged(&[0x03, 0]),
            Err(ProtocolError::InvalidLoginAcknowledged)
        );
    }

    #[test]
    fn login_acknowledged_rejects_empty_and_wrong_packet_ids() {
        assert_eq!(
            parse_login_acknowledged(&[]),
            Err(ProtocolError::InvalidLoginAcknowledged)
        );
        assert_eq!(
            parse_login_acknowledged(&[0x02]),
            Err(ProtocolError::InvalidLoginAcknowledged)
        );
    }

    #[test]
    fn configuration_registry_fixture_has_expected_packet_header() {
        let packet = [
            0x0e, 0x01, 0x09, b'm', b'i', b'n', b'e', b'c', b'r', b'a', b'f', b't', 0x04, b'c',
            b'o', b'r', b'e', 0x06, b'2', b'6', b'.', b'1', b'.', b'2',
        ];
        let mut input = bytes::BytesMut::from(&packet[..]);
        let decoded = decode_frame(&mut input).unwrap().unwrap();
        assert_eq!(decoded.as_ref(), &packet[1..15]);
        assert_eq!(input.as_ref(), &packet[15..]);
    }
}
