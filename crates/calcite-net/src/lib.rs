use bytes::BytesMut;
use calcite_protocol::{ProtocolError, decode_frame, read_var_i32};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Handshake,
    Status,
    Login,
    Configuration,
    Play,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ConnectionError {
    #[error("invalid connection state transition from {from:?} to {to:?}")]
    InvalidTransition {
        from: ConnectionState,
        to: ConnectionState,
    },
}

impl ConnectionState {
    pub fn transition(self, next: Self) -> Result<Self, ConnectionError> {
        let valid = matches!(
            (self, next),
            (Self::Handshake, Self::Status | Self::Login)
                | (Self::Login, Self::Configuration)
                | (Self::Configuration, Self::Play)
                | (_, Self::Closed)
        );
        if valid {
            Ok(next)
        } else {
            Err(ConnectionError::InvalidTransition {
                from: self,
                to: next,
            })
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Connection {
    state: ConnectionState,
}

impl Default for Connection {
    fn default() -> Self {
        Self::new()
    }
}

impl Connection {
    pub const fn new() -> Self {
        Self {
            state: ConnectionState::Handshake,
        }
    }
    pub const fn state(self) -> ConnectionState {
        self.state
    }

    pub fn transition(&mut self, next: ConnectionState) -> Result<(), ConnectionError> {
        self.state = self.state.transition(next)?;
        Ok(())
    }
}

pub struct FrameDecoder {
    buffer: BytesMut,
    max_frame: usize,
}

impl FrameDecoder {
    pub fn new(max_frame: usize) -> Self {
        Self {
            buffer: BytesMut::new(),
            max_frame,
        }
    }
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<BytesMut>, ProtocolError> {
        let mut frames = Vec::new();
        for byte in bytes {
            self.buffer.extend_from_slice(std::slice::from_ref(byte));
            while let Some(frame) = decode_frame(&mut self.buffer)? {
                if frame.len() > self.max_frame {
                    return Err(ProtocolError::PacketTooLarge);
                }
                frames.push(frame);
            }

            let mut header = &self.buffer[..];
            if let Ok(length) = read_var_i32(&mut header) {
                if length < 0 || length as usize > self.max_frame {
                    return Err(ProtocolError::PacketTooLarge);
                }
            }
        }
        Ok(frames)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_accepts_protocol_state_flow() {
        let mut connection = Connection::new();
        connection.transition(ConnectionState::Login).unwrap();
        connection
            .transition(ConnectionState::Configuration)
            .unwrap();
        connection.transition(ConnectionState::Play).unwrap();
        assert_eq!(connection.state(), ConnectionState::Play);
    }

    #[test]
    fn connection_rejects_invalid_state_flow() {
        let mut connection = Connection::new();
        assert_eq!(
            connection.transition(ConnectionState::Play),
            Err(ConnectionError::InvalidTransition {
                from: ConnectionState::Handshake,
                to: ConnectionState::Play,
            })
        );
        assert_eq!(connection.state(), ConnectionState::Handshake);
    }

    #[test]
    fn frame_decoder_handles_multiple_frames_and_partial_tail() {
        let first = calcite_protocol::encode_frame(b"one").unwrap();
        let second = calcite_protocol::encode_frame(b"two").unwrap();
        let mut input = first.to_vec();
        input.extend_from_slice(&second[..2]);
        let mut decoder = FrameDecoder::new(16);

        let frames = decoder.push(&input).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(&frames[0][..], b"one");
        assert_eq!(decoder.push(&second[2..]).unwrap().len(), 1);
    }

    #[test]
    fn frame_decoder_rejects_payload_over_limit() {
        let frame = calcite_protocol::encode_frame(b"oversized").unwrap();
        let mut decoder = FrameDecoder::new(4);

        assert_eq!(decoder.push(&frame), Err(ProtocolError::PacketTooLarge));
    }

    #[test]
    fn frame_decoder_rejects_oversized_declared_length_before_payload_arrives() {
        let mut decoder = FrameDecoder::new(4);

        assert_eq!(decoder.push(&[5]), Err(ProtocolError::PacketTooLarge));
    }
}
