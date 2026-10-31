//! Minecraft protocol primitives for the protocol-775 migration target.

use bytes::{BufMut, BytesMut};
use md5::{Digest, Md5};

pub const PROTOCOL_VERSION: i32 = 775;
pub const MINECRAFT_VERSION: &str = "26.1.2";
pub const CONFIGURATION_CLIENTBOUND_DISCONNECT_PACKET_ID: i32 = 0x02;
pub const CONFIGURATION_SERVERBOUND_CLIENT_INFORMATION_PACKET_ID: i32 = 0x00;
pub const CONFIGURATION_SERVERBOUND_FINISH_ACK_PACKET_ID: i32 = 0x03;
pub const PLAY_SERVERBOUND_CLIENT_TICK_END_PACKET_ID: i32 = 0x0D;
pub const PLAY_SERVERBOUND_KEEP_ALIVE_PACKET_ID: i32 = 0x1C;
pub const PLAY_CLIENTBOUND_DISCONNECT_PACKET_ID: i32 = 0x20;
pub const PLAY_CLIENTBOUND_KEEP_ALIVE_PACKET_ID: i32 = 0x2C;
/// Protocol limit for the Client Information locale, in characters.
pub const CLIENT_INFORMATION_MAX_LOCALE_CHARS: usize = 16;
const NBT_STRING_TAG: u8 = 8;

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
    #[error("invalid play packet id {0}")]
    InvalidPlayPacketId(i32),
    #[error("play keep-alive payload is malformed")]
    InvalidPlayKeepAlive,
    #[error("play keep-alive payload does not match the pending challenge")]
    KeepAlivePayloadMismatch,
    #[error("invalid configuration packet id {0}")]
    InvalidConfigurationPacketId(i32),
    #[error("client information locale is malformed")]
    InvalidClientLocale,
    #[error("client information field {0} has an invalid value")]
    InvalidClientInformationField(&'static str),
    #[error("disconnect reason is too long")]
    DisconnectReasonTooLong,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatMode {
    Enabled,
    CommandsOnly,
    Hidden,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MainHand {
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParticleStatus {
    All,
    Decreased,
    Minimal,
}

/// Serverbound Client Information (Configuration 0x00).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientInformation {
    pub locale: String,
    /// Raw requested view distance as sent by the client.
    pub view_distance: u8,
    pub chat_mode: ChatMode,
    pub chat_colors: bool,
    pub displayed_skin_parts: u8,
    pub main_hand: MainHand,
    pub text_filtering: bool,
    pub allow_server_listings: bool,
    pub particle_status: ParticleStatus,
}

impl ClientInformation {
    /// View distance used by the server; the C++ oracle clamps the raw
    /// unsigned byte to 2..=32.
    pub fn effective_view_distance(&self) -> u8 {
        self.view_distance.clamp(2, 32)
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerboundKeepAlive {
    pub payload: u64,
}

/// Parse the protocol-775 Play serverbound Keep Alive packet.
/// The C++ oracle routes packet id 0x1C and reads one big-endian u64.
pub fn parse_play_serverbound_keep_alive(
    mut payload: &[u8],
) -> Result<ServerboundKeepAlive, ProtocolError> {
    let packet_id = read_var_i32(&mut payload)?;
    if packet_id != PLAY_SERVERBOUND_KEEP_ALIVE_PACKET_ID {
        return Err(ProtocolError::InvalidPlayPacketId(packet_id));
    }
    if payload.len() != 8 {
        return Err(ProtocolError::InvalidPlayKeepAlive);
    }
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(payload);
    Ok(ServerboundKeepAlive {
        payload: u64::from_be_bytes(bytes),
    })
}

/// Encode the protocol-775 Play clientbound Keep Alive packet.
pub fn encode_play_clientbound_keep_alive(payload: u64) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(9);
    write_var_i32(PLAY_CLIENTBOUND_KEEP_ALIVE_PACKET_ID, &mut encoded);
    encoded.extend_from_slice(&payload.to_be_bytes());
    encoded
}

/// Encode Play Disconnect (0x20) with a network-NBT string reason, matching
/// `disconnect::play` in the C++ oracle.
pub fn encode_play_disconnect(reason: &str) -> Result<Vec<u8>, ProtocolError> {
    encode_nbt_string_disconnect(PLAY_CLIENTBOUND_DISCONNECT_PACKET_ID, reason)
}

/// Encode Configuration Disconnect (0x02), matching
/// `disconnect::configuration` in the C++ oracle.
pub fn encode_configuration_disconnect(reason: &str) -> Result<Vec<u8>, ProtocolError> {
    encode_nbt_string_disconnect(CONFIGURATION_CLIENTBOUND_DISCONNECT_PACKET_ID, reason)
}

fn encode_nbt_string_disconnect(packet_id: i32, reason: &str) -> Result<Vec<u8>, ProtocolError> {
    // The oracle asserts < 65500 bytes; Java's modified UTF-8 caps at u16.
    let len = u16::try_from(reason.len()).map_err(|_| ProtocolError::DisconnectReasonTooLong)?;
    let mut encoded = Vec::with_capacity(4 + reason.len());
    write_var_i32(packet_id, &mut encoded);
    encoded.push(NBT_STRING_TAG);
    encoded.extend_from_slice(&len.to_be_bytes());
    encoded.extend_from_slice(reason.as_bytes());
    Ok(encoded)
}

/// Parse Configuration Client Information. The field layout is the
/// 1.21.2+ layout (locale, view distance, chat mode, chat colors, skin
/// parts, main hand, text filtering, server listings, particle status).
/// The C++ oracle reads only locale and view distance; this parser also
/// rejects truncated, out-of-range and trailing data.
pub fn parse_configuration_client_information(
    mut payload: &[u8],
) -> Result<ClientInformation, ProtocolError> {
    let packet_id = read_var_i32(&mut payload)?;
    if packet_id != CONFIGURATION_SERVERBOUND_CLIENT_INFORMATION_PACKET_ID {
        return Err(ProtocolError::InvalidConfigurationPacketId(packet_id));
    }
    let locale_len = read_var_i32(&mut payload)?;
    let max_bytes = CLIENT_INFORMATION_MAX_LOCALE_CHARS * 4;
    let locale_len = usize::try_from(locale_len)
        .ok()
        .filter(|len| *len <= max_bytes)
        .ok_or(ProtocolError::InvalidClientLocale)?;
    let locale_bytes = take(&mut payload, locale_len)?;
    let locale =
        std::str::from_utf8(locale_bytes).map_err(|_| ProtocolError::InvalidClientLocale)?;
    if locale.chars().count() > CLIENT_INFORMATION_MAX_LOCALE_CHARS {
        return Err(ProtocolError::InvalidClientLocale);
    }
    let view_distance = take(&mut payload, 1)?[0];
    let chat_mode = match read_var_i32(&mut payload)? {
        0 => ChatMode::Enabled,
        1 => ChatMode::CommandsOnly,
        2 => ChatMode::Hidden,
        _ => return Err(ProtocolError::InvalidClientInformationField("chat mode")),
    };
    let chat_colors = read_bool(&mut payload, "chat colors")?;
    let displayed_skin_parts = take(&mut payload, 1)?[0];
    let main_hand = match read_var_i32(&mut payload)? {
        0 => MainHand::Left,
        1 => MainHand::Right,
        _ => return Err(ProtocolError::InvalidClientInformationField("main hand")),
    };
    let text_filtering = read_bool(&mut payload, "text filtering")?;
    let allow_server_listings = read_bool(&mut payload, "allow server listings")?;
    let particle_status = match read_var_i32(&mut payload)? {
        0 => ParticleStatus::All,
        1 => ParticleStatus::Decreased,
        2 => ParticleStatus::Minimal,
        _ => {
            return Err(ProtocolError::InvalidClientInformationField(
                "particle status",
            ));
        }
    };
    if !payload.is_empty() {
        return Err(ProtocolError::TrailingData);
    }
    Ok(ClientInformation {
        locale: locale.to_owned(),
        view_distance,
        chat_mode,
        chat_colors,
        displayed_skin_parts,
        main_hand,
        text_filtering,
        allow_server_listings,
        particle_status,
    })
}

/// Read the packet id of an already framed payload without consuming it.
pub fn peek_packet_id(payload: &[u8]) -> Result<i32, ProtocolError> {
    let mut view = payload;
    read_var_i32(&mut view)
}

fn take<'a>(buf: &mut &'a [u8], len: usize) -> Result<&'a [u8], ProtocolError> {
    if buf.len() < len {
        return Err(ProtocolError::Truncated);
    }
    let (head, tail) = buf.split_at(len);
    *buf = tail;
    Ok(head)
}

fn read_bool(buf: &mut &[u8], field: &'static str) -> Result<bool, ProtocolError> {
    match take(buf, 1)?[0] {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(ProtocolError::InvalidClientInformationField(field)),
    }
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

    #[test]
    fn play_keep_alive_matches_cpp_ids_and_big_endian_layout() {
        let payload = 0x0102_0304_0506_0708u64;
        let encoded = encode_play_clientbound_keep_alive(payload);
        assert_eq!(encoded, vec![0x2c, 1, 2, 3, 4, 5, 6, 7, 8]);

        let mut serverbound = vec![0x1c];
        serverbound.extend_from_slice(&payload.to_be_bytes());
        assert_eq!(
            parse_play_serverbound_keep_alive(&serverbound),
            Ok(ServerboundKeepAlive { payload })
        );
    }

    fn client_information_bytes(locale: &[u8], view_distance: u8) -> Vec<u8> {
        let mut out = vec![0x00];
        write_var_i32(locale.len() as i32, &mut out);
        out.extend_from_slice(locale);
        out.push(view_distance);
        write_var_i32(0, &mut out); // chat mode: enabled
        out.push(1); // chat colors
        out.push(0x7f); // skin parts
        write_var_i32(1, &mut out); // main hand: right
        out.push(0); // text filtering
        out.push(1); // allow server listings
        write_var_i32(0, &mut out); // particle status: all
        out
    }

    #[test]
    fn parses_vanilla_client_information() {
        let bytes = client_information_bytes(b"en_us", 12);
        assert_eq!(
            bytes,
            [
                0x00, 0x05, b'e', b'n', b'_', b'u', b's', 12, 0, 1, 0x7f, 1, 0, 1, 0
            ]
        );
        let info = parse_configuration_client_information(&bytes).unwrap();
        assert_eq!(
            info,
            ClientInformation {
                locale: "en_us".into(),
                view_distance: 12,
                chat_mode: ChatMode::Enabled,
                chat_colors: true,
                displayed_skin_parts: 0x7f,
                main_hand: MainHand::Right,
                text_filtering: false,
                allow_server_listings: true,
                particle_status: ParticleStatus::All,
            }
        );
        assert_eq!(info.effective_view_distance(), 12);
    }

    #[test]
    fn client_information_view_distance_is_clamped_like_cpp() {
        for (raw, expected) in [(0, 2), (1, 2), (2, 2), (32, 32), (33, 32), (255, 32)] {
            let info =
                parse_configuration_client_information(&client_information_bytes(b"en_us", raw))
                    .unwrap();
            assert_eq!(info.view_distance, raw);
            assert_eq!(info.effective_view_distance(), expected, "raw={raw}");
        }
    }

    #[test]
    fn client_information_rejects_every_truncation_and_trailing_data() {
        let bytes = client_information_bytes(b"en_us", 8);
        for len in 1..bytes.len() {
            assert!(
                parse_configuration_client_information(&bytes[..len]).is_err(),
                "len={len}"
            );
        }
        let mut trailing = bytes;
        trailing.push(0);
        assert_eq!(
            parse_configuration_client_information(&trailing),
            Err(ProtocolError::TrailingData)
        );
    }

    #[test]
    fn client_information_rejects_bad_fields() {
        assert_eq!(
            parse_configuration_client_information(&[0x03]),
            Err(ProtocolError::InvalidConfigurationPacketId(3))
        );
        assert_eq!(
            parse_configuration_client_information(&client_information_bytes(&[0xff], 8)),
            Err(ProtocolError::InvalidClientLocale)
        );
        assert_eq!(
            parse_configuration_client_information(&client_information_bytes(&[b'a'; 17], 8)),
            Err(ProtocolError::InvalidClientLocale)
        );
        assert_eq!(
            parse_configuration_client_information(&[0x00, 0x7f]),
            Err(ProtocolError::InvalidClientLocale)
        );
        // Offsets: id(1) + len(1) + "en_us"(5) + view(1) = 8 -> chat mode.
        let cases: [(usize, u8, &str); 6] = [
            (8, 3, "chat mode"),
            (9, 2, "chat colors"),
            (11, 2, "main hand"),
            (12, 2, "text filtering"),
            (13, 2, "allow server listings"),
            (14, 3, "particle status"),
        ];
        for (offset, value, field) in cases {
            let mut bytes = client_information_bytes(b"en_us", 8);
            bytes[offset] = value;
            assert_eq!(
                parse_configuration_client_information(&bytes),
                Err(ProtocolError::InvalidClientInformationField(field)),
                "field={field}"
            );
        }
    }

    #[test]
    fn disconnect_packets_match_cpp_nbt_string_layout() {
        assert_eq!(
            encode_play_disconnect("Bye").unwrap(),
            [0x20, 0x08, 0x00, 0x03, b'B', b'y', b'e']
        );
        assert_eq!(
            encode_configuration_disconnect("").unwrap(),
            [0x02, 0x08, 0x00, 0x00]
        );
        assert_eq!(
            encode_play_disconnect(&"x".repeat(65536)),
            Err(ProtocolError::DisconnectReasonTooLong)
        );
    }

    #[test]
    fn play_keep_alive_rejects_malformed_payloads() {
        assert_eq!(
            parse_play_serverbound_keep_alive(&[0x1b, 0, 0, 0, 0, 0, 0, 0, 1]),
            Err(ProtocolError::InvalidPlayPacketId(0x1b))
        );
        assert_eq!(
            parse_play_serverbound_keep_alive(&[0x1c, 0, 0]),
            Err(ProtocolError::InvalidPlayKeepAlive)
        );
        let mut trailing = vec![0x1c];
        trailing.extend_from_slice(&1u64.to_be_bytes());
        trailing.push(0);
        assert_eq!(
            parse_play_serverbound_keep_alive(&trailing),
            Err(ProtocolError::InvalidPlayKeepAlive)
        );
    }
}
