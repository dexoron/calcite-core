use bytes::BytesMut;
use calcite_protocol::{
    ClientInformation, LoginStart, ProtocolError, ServerboundKeepAlive, decode_frame,
    encode_login_success, parse_configuration_acknowledged, parse_configuration_client_information,
    parse_login_acknowledged, parse_login_start, parse_play_serverbound_keep_alive, read_var_i32,
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
    #[error(transparent)]
    KeepAlive(#[from] KeepAliveError),
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
        self.transition(ConnectionState::Play)
    }

    /// Parse serverbound Client Information while in Configuration.
    pub fn parse_client_information(
        &self,
        payload: &[u8],
    ) -> Result<ClientInformation, ConnectionError> {
        if self.state != ConnectionState::Configuration {
            return Err(ConnectionError::InvalidTransition {
                from: self.state,
                to: ConnectionState::Configuration,
            });
        }
        Ok(parse_configuration_client_information(payload)?)
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

    /// Enter the typed Play boundary and validate one serverbound Keep Alive.
    /// The expected payload is the outstanding challenge generated by the
    /// clientbound 0x2C packet.
    pub fn handle_play_keep_alive(
        &self,
        payload: &[u8],
        expected_payload: u64,
    ) -> Result<ServerboundKeepAlive, ConnectionError> {
        if self.state != ConnectionState::Play {
            return Err(ConnectionError::InvalidTransition {
                from: self.state,
                to: ConnectionState::Play,
            });
        }
        let keep_alive = parse_play_serverbound_keep_alive(payload)?;
        if keep_alive.payload != expected_payload {
            return Err(ConnectionError::Protocol(
                ProtocolError::KeepAlivePayloadMismatch,
            ));
        }
        Ok(keep_alive)
    }

    /// Validate a serverbound Keep Alive against every pending challenge of
    /// `tracker`, as the C++ oracle does with its `active_payloads` slots.
    pub fn handle_play_keep_alive_reply(
        &self,
        payload: &[u8],
        tracker: &mut KeepAliveTracker,
    ) -> Result<ServerboundKeepAlive, ConnectionError> {
        if self.state != ConnectionState::Play {
            return Err(ConnectionError::InvalidTransition {
                from: self.state,
                to: ConnectionState::Play,
            });
        }
        let keep_alive = parse_play_serverbound_keep_alive(payload)?;
        tracker.acknowledge(keep_alive.payload)?;
        Ok(keep_alive)
    }
}

/// Maximum number of unanswered Keep Alive challenges kept per connection,
/// matching the C++ oracle's `active_payloads` array.
pub const KEEP_ALIVE_SLOTS: usize = 10;
/// Number of consecutive send ticks without a reply that are tolerated. The
/// oracle disconnects when `timeout_counter++ > 2`, i.e. on the fourth tick.
pub const KEEP_ALIVE_MAX_MISSED_TICKS: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum KeepAliveError {
    #[error("client did not answer Keep Alive in time")]
    Timeout,
    #[error("Keep Alive reply does not match any pending challenge")]
    CorrelationMismatch,
}

/// Outstanding Keep Alive challenges of one Play connection.
///
/// Invariants: payload `0` is never issued (it marks an empty slot, as in the
/// oracle) and at most [`KEEP_ALIVE_SLOTS`] challenges are tracked; when all
/// slots are full the first one is replaced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeepAliveTracker {
    pending: [u64; KEEP_ALIVE_SLOTS],
    missed_ticks: u32,
}

impl Default for KeepAliveTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl KeepAliveTracker {
    pub const fn new() -> Self {
        Self {
            pending: [0; KEEP_ALIVE_SLOTS],
            missed_ticks: 0,
        }
    }

    /// Register a send tick and the challenge sent on it. Fails with
    /// [`KeepAliveError::Timeout`] once too many ticks passed without a reply;
    /// in that case no challenge should be sent.
    /// Returns the payload actually registered, which must be sent to the
    /// client (`0` is mapped to `1`).
    pub fn issue(&mut self, payload: u64) -> Result<u64, KeepAliveError> {
        if self.missed_ticks >= KEEP_ALIVE_MAX_MISSED_TICKS {
            return Err(KeepAliveError::Timeout);
        }
        self.missed_ticks += 1;
        let payload = payload.max(1);
        match self.pending.iter_mut().find(|slot| **slot == 0) {
            Some(slot) => *slot = payload,
            None => self.pending[0] = payload,
        }
        Ok(payload)
    }

    /// Match a client reply against the pending challenges.
    pub fn acknowledge(&mut self, payload: u64) -> Result<(), KeepAliveError> {
        if payload == 0 {
            return Err(KeepAliveError::CorrelationMismatch);
        }
        let slot = self
            .pending
            .iter_mut()
            .find(|slot| **slot == payload)
            .ok_or(KeepAliveError::CorrelationMismatch)?;
        *slot = 0;
        self.missed_ticks = 0;
        Ok(())
    }

    pub fn pending_count(&self) -> usize {
        self.pending.iter().filter(|slot| **slot != 0).count()
    }
}

/// Non-cryptographic generator for Keep Alive challenges. The oracle uses
/// `std::minstd_rand`; challenges only need to be hard to guess by accident,
/// so a randomly seeded xorshift is sufficient and avoids a new dependency.
#[derive(Debug, Clone)]
pub struct KeepAlivePayloads {
    state: u64,
}

impl Default for KeepAlivePayloads {
    fn default() -> Self {
        use std::hash::{BuildHasher, Hasher};
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u64(0x9e37_79b9_7f4a_7c15);
        Self::with_seed(hasher.finish())
    }
}

impl KeepAlivePayloads {
    pub fn with_seed(seed: u64) -> Self {
        Self { state: seed.max(1) }
    }

    /// Next non-zero payload.
    pub fn next_payload(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
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
        assert_eq!(connection.acknowledge_configuration(&[0x03]), Ok(()));
        assert_eq!(connection.state(), ConnectionState::Play);
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

    #[test]
    fn play_keep_alive_requires_play_state_and_exact_payload() {
        let mut connection = Connection::new();
        assert!(matches!(
            connection.handle_play_keep_alive(&[0x1c; 9], 7),
            Err(ConnectionError::InvalidTransition { .. })
        ));
        connection.transition(ConnectionState::Login).unwrap();
        connection
            .transition(ConnectionState::Configuration)
            .unwrap();
        connection.transition(ConnectionState::Play).unwrap();

        let mut packet = vec![0x1c];
        packet.extend_from_slice(&7u64.to_be_bytes());
        assert_eq!(
            connection
                .handle_play_keep_alive(&packet, 7)
                .unwrap()
                .payload,
            7
        );
        assert_eq!(
            connection.handle_play_keep_alive(&packet, 8),
            Err(ConnectionError::Protocol(
                ProtocolError::KeepAlivePayloadMismatch
            ))
        );
    }

    #[test]
    fn client_information_requires_configuration_state() {
        let info = [
            0x00, 0x05, b'e', b'n', b'_', b'u', b's', 12, 0, 1, 0x7f, 1, 0, 1, 0,
        ];
        let mut connection = Connection::new();
        assert!(matches!(
            connection.parse_client_information(&info),
            Err(ConnectionError::InvalidTransition { .. })
        ));
        connection.transition(ConnectionState::Login).unwrap();
        connection
            .transition(ConnectionState::Configuration)
            .unwrap();
        assert_eq!(
            connection.parse_client_information(&info).unwrap().locale,
            "en_us"
        );
        assert_eq!(
            connection.parse_client_information(&info[..5]),
            Err(ConnectionError::Protocol(ProtocolError::Truncated))
        );
    }

    #[test]
    fn keep_alive_tracker_matches_any_pending_challenge_once() {
        let mut tracker = KeepAliveTracker::new();
        tracker.issue(11).unwrap();
        tracker.issue(22).unwrap();
        assert_eq!(tracker.pending_count(), 2);
        assert_eq!(tracker.acknowledge(22), Ok(()));
        assert_eq!(
            tracker.acknowledge(22),
            Err(KeepAliveError::CorrelationMismatch)
        );
        assert_eq!(tracker.acknowledge(11), Ok(()));
        assert_eq!(
            tracker.acknowledge(0),
            Err(KeepAliveError::CorrelationMismatch)
        );
        assert_eq!(tracker.pending_count(), 0);
    }

    #[test]
    fn keep_alive_tracker_times_out_after_missed_ticks_and_resets_on_reply() {
        let mut tracker = KeepAliveTracker::new();
        for payload in 1..=KEEP_ALIVE_MAX_MISSED_TICKS as u64 {
            tracker.issue(payload).unwrap();
        }
        assert_eq!(tracker.issue(99), Err(KeepAliveError::Timeout));

        let mut tracker = KeepAliveTracker::new();
        tracker.issue(1).unwrap();
        tracker.issue(2).unwrap();
        tracker.acknowledge(1).unwrap();
        for payload in 3..3 + KEEP_ALIVE_MAX_MISSED_TICKS as u64 {
            tracker.issue(payload).unwrap();
        }
        assert_eq!(tracker.issue(99), Err(KeepAliveError::Timeout));
    }

    #[test]
    fn keep_alive_tracker_replaces_first_slot_when_full_and_never_stores_zero() {
        let mut tracker = KeepAliveTracker::new();
        for payload in 1..=KEEP_ALIVE_SLOTS as u64 {
            tracker.issue(payload).unwrap();
            tracker.missed_ticks = 0;
        }
        tracker.issue(100).unwrap();
        assert_eq!(tracker.pending_count(), KEEP_ALIVE_SLOTS);
        assert_eq!(
            tracker.acknowledge(1),
            Err(KeepAliveError::CorrelationMismatch)
        );
        assert_eq!(tracker.acknowledge(100), Ok(()));

        let mut tracker = KeepAliveTracker::new();
        assert_eq!(tracker.issue(0), Ok(1));
        assert_eq!(tracker.acknowledge(1), Ok(()));
    }

    #[test]
    fn keep_alive_payloads_are_non_zero_and_seed_deterministic() {
        let mut a = KeepAlivePayloads::with_seed(42);
        let mut b = KeepAlivePayloads::with_seed(42);
        for _ in 0..1000 {
            let value = a.next_payload();
            assert_ne!(value, 0);
            assert_eq!(value, b.next_payload());
        }
        assert_ne!(KeepAlivePayloads::with_seed(0).next_payload(), 0);
    }

    #[test]
    fn play_keep_alive_rejects_wrong_id_truncation_and_trailing_data() {
        let mut connection = Connection::new();
        connection.transition(ConnectionState::Login).unwrap();
        connection
            .transition(ConnectionState::Configuration)
            .unwrap();
        connection.transition(ConnectionState::Play).unwrap();
        assert_eq!(
            connection.handle_play_keep_alive(&[0x1b, 0, 0, 0, 0, 0, 0, 0, 1], 1),
            Err(ConnectionError::Protocol(
                ProtocolError::InvalidPlayPacketId(0x1b)
            ))
        );
        assert_eq!(
            connection.handle_play_keep_alive(&[0x1c, 0, 0], 1),
            Err(ConnectionError::Protocol(
                ProtocolError::InvalidPlayKeepAlive
            ))
        );
        let mut trailing = vec![0x1c];
        trailing.extend_from_slice(&1u64.to_be_bytes());
        trailing.push(0);
        assert_eq!(
            connection.handle_play_keep_alive(&trailing, 1),
            Err(ConnectionError::Protocol(
                ProtocolError::InvalidPlayKeepAlive
            ))
        );
    }
}
