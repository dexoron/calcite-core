use bytes::BytesMut;
use calcite_protocol::{ProtocolError, decode_frame};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Handshake,
    Status,
    Login,
    Configuration,
    Play,
    Closed,
}

impl ConnectionState {
    pub fn transition(self, next: Self) -> Result<Self, ProtocolError> {
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
            Err(ProtocolError::TrailingData)
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

    pub fn transition(&mut self, next: ConnectionState) -> Result<(), ProtocolError> {
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
        self.buffer.extend_from_slice(bytes);
        let mut frames = Vec::new();
        while let Some(frame) = decode_frame(&mut self.buffer)? {
            if frame.len() > self.max_frame {
                return Err(ProtocolError::PacketTooLarge);
            }
            frames.push(frame);
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
        assert!(connection.transition(ConnectionState::Play).is_err());
        assert_eq!(connection.state(), ConnectionState::Handshake);
    }
}
