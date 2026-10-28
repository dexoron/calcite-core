//! Minecraft protocol primitives for the protocol-775 migration target.

use bytes::{Buf, BufMut, BytesMut};

pub const PROTOCOL_VERSION: i32 = 775;
pub const MINECRAFT_VERSION: &str = "26.1.2";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ProtocolError {
    #[error("VarInt exceeds five bytes")]
    VarIntTooBig,
    #[error("packet length exceeds configured maximum")]
    PacketTooLarge,
    #[error("packet is truncated")]
    Truncated,
    #[error("packet contains unexpected trailing data")]
    TrailingData,
}

pub fn read_var_i32(buf: &mut &[u8]) -> Result<i32, ProtocolError> {
    let mut result = 0u32;
    for shift in (0..35).step_by(7) {
        let byte = *buf.first().ok_or(ProtocolError::Truncated)?;
        *buf = &buf[1..];
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
        Err(ProtocolError::Truncated) => return Ok(None),
        Err(error) => return Err(error),
        _ => unreachable!(),
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
}
