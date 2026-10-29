use bytes::BytesMut;
use calcite_protocol::{
    LoginStart, ProtocolError, decode_frame, encode_login_success,
    parse_configuration_acknowledged, parse_login_acknowledged, parse_login_start, read_var_i32,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Handshake,
    Status,
    Login,
    Configuration,
    Play,
    Closed,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ConnectionError {
    #[error("invalid connection state transition from {from:?} to {to:?}")]
    InvalidTransition {
        from: ConnectionState,
        to: ConnectionState,
    },
    #[error("protocol error while parsing login start: {0}")]
    Protocol(#[from] ProtocolError),
    #[error("offline UUID derivation is unavailable; Login Success is not emitted")]
    OfflineUuidUnavailable,
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

    pub fn parse_login_start(&self, payload: &[u8]) -> Result<LoginStart, ConnectionError> {
        if self.state != ConnectionState::Login {
            return Err(ConnectionError::InvalidTransition {
                from: self.state,
                to: ConnectionState::Login,
            });
        }
        Ok(parse_login_start(payload)?)
    }

    pub fn login_success(&self, login: &LoginStart) -> Result<Vec<u8>, ConnectionError> {
        encode_login_success(login).map_err(ConnectionError::Protocol)
    }

    pub fn acknowledge_login(&mut self, payload: &[u8]) -> Result<(), ConnectionError> {
        if self.state != ConnectionState::Login {
            return Err(ConnectionError::InvalidTransition {
                from: self.state,
                to: ConnectionState::Login,
            });
        }
        parse_login_acknowledged(payload).map_err(ConnectionError::Protocol)?;
        self.transition(ConnectionState::Configuration)
    }

    pub fn configuration_finish_packet(&self) -> Result<Vec<u8>, ConnectionError> {
        if self.state != ConnectionState::Configuration {
            return Err(ConnectionError::InvalidTransition {
                from: self.state,
                to: ConnectionState::Configuration,
            });
        }
        Ok(calcite_protocol::encode_configuration_finish())
    }

    pub fn acknowledge_configuration(&mut self, payload: &[u8]) -> Result<(), ConnectionError> {
        if self.state != ConnectionState::Configuration {
            return Err(ConnectionError::InvalidTransition {
                from: self.state,
                to: ConnectionState::Configuration,
            });
        }
        parse_configuration_acknowledged(payload).map_err(ConnectionError::Protocol)?;
        Err(ConnectionError::InvalidTransition {
            from: ConnectionState::Configuration,
            to: ConnectionState::Play,
        })
    }

    pub fn validate_configuration_ack(&self, payload: &[u8]) -> Result<(), ConnectionError> {
        if self.state != ConnectionState::Configuration {
            return Err(ConnectionError::InvalidTransition {
                from: self.state,
                to: ConnectionState::Configuration,
            });
        }
        parse_configuration_acknowledged(payload).map_err(ConnectionError::Protocol)
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
    fn login_start_and_success_are_supported() {
        let mut connection = Connection::new();
        connection.transition(ConnectionState::Login).unwrap();
        let mut payload = vec![0, 5];
        payload.extend_from_slice(b"Steve");
        payload.extend_from_slice(&[7; 16]);
        let login = connection.parse_login_start(&payload).unwrap();
        assert_eq!(login.name, "Steve");
        let success = connection.login_success(&login).unwrap();
        assert_eq!(success[0], 0x02);
        assert_eq!(connection.acknowledge_login(&[0x03]), Ok(()));
        assert_eq!(connection.state(), ConnectionState::Configuration);
        assert_eq!(
            connection.configuration_finish_packet().unwrap(),
            vec![0x03]
        );
        assert!(matches!(
            connection.acknowledge_configuration(&[0x03]),
            Err(ConnectionError::InvalidTransition { .. })
        ));
        assert_eq!(connection.state(), ConnectionState::Configuration);
    }

    #[test]
    fn login_start_requires_login_state_and_rejects_malformed_payload() {
        let connection = Connection::new();
        assert!(matches!(
            connection.parse_login_start(&[0]),
            Err(ConnectionError::InvalidTransition { .. })
        ));
        let mut connection = Connection::new();
        connection.transition(ConnectionState::Login).unwrap();
        assert_eq!(
            connection.parse_login_start(&[0, 0]),
            Err(ConnectionError::Protocol(ProtocolError::InvalidPlayerName))
        );
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
    fn frame_decoder_accepts_status_fixtures_at_every_split_boundary() {
        let fixture = [0x01, 0x00, 0x09, 0x01, 1, 2, 3, 4, 5, 6, 7, 8];
        for split in 0..=fixture.len() {
            let mut decoder = FrameDecoder::new(16);
            let mut frames = decoder.push(&fixture[..split]).unwrap();
            frames.extend(decoder.push(&fixture[split..]).unwrap());
            assert_eq!(frames.len(), 2, "split={split}");
            assert_eq!(frames[0].as_ref(), &[0]);
            assert_eq!(frames[1].as_ref(), &[0x01, 1, 2, 3, 4, 5, 6, 7, 8]);
        }
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
