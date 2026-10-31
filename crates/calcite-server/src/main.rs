use std::time::Duration;

use anyhow::{Context, Result};
use bytes::BytesMut;
use calcite_net::{Connection, ConnectionState, FrameDecoder, KeepAlivePayloads, KeepAliveTracker};
use calcite_protocol::{
    CONFIGURATION_SERVERBOUND_CLIENT_INFORMATION_PACKET_ID,
    CONFIGURATION_SERVERBOUND_FINISH_ACK_PACKET_ID, ClientInformation, HandshakeIntent,
    PLAY_SERVERBOUND_CLIENT_TICK_END_PACKET_ID, PLAY_SERVERBOUND_KEEP_ALIVE_PACKET_ID,
    encode_configuration_disconnect, encode_frame, encode_play_clientbound_keep_alive,
    encode_play_disconnect, parse_handshake, peek_packet_id, write_var_i32,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const DEFAULT_MAX_FRAME: usize = 2 * 1024 * 1024;
const DEFAULT_KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(1);
const KEEP_ALIVE_TIMEOUT_REASON: &str = "Timeout (powered by ActualKlasterKraft)";

const CONFIGURATION_FIXTURES: [&[u8]; 30] = [
    include_bytes!("../../../docs/configuration/packet-0"),
    include_bytes!("../../../docs/configuration/packet-1"),
    include_bytes!("../../../docs/configuration/packet-2"),
    include_bytes!("../../../docs/configuration/packet-3"),
    include_bytes!("../../../docs/configuration/packet-4"),
    include_bytes!("../../../docs/configuration/packet-5"),
    include_bytes!("../../../docs/configuration/packet-6"),
    include_bytes!("../../../docs/configuration/packet-7"),
    include_bytes!("../../../docs/configuration/packet-8"),
    include_bytes!("../../../docs/configuration/packet-9"),
    include_bytes!("../../../docs/configuration/packet-10"),
    include_bytes!("../../../docs/configuration/packet-11"),
    include_bytes!("../../../docs/configuration/packet-12"),
    include_bytes!("../../../docs/configuration/packet-13"),
    include_bytes!("../../../docs/configuration/packet-14"),
    include_bytes!("../../../docs/configuration/packet-15"),
    include_bytes!("../../../docs/configuration/packet-16"),
    include_bytes!("../../../docs/configuration/packet-17"),
    include_bytes!("../../../docs/configuration/packet-18"),
    include_bytes!("../../../docs/configuration/packet-19"),
    include_bytes!("../../../docs/configuration/packet-20"),
    include_bytes!("../../../docs/configuration/packet-21"),
    include_bytes!("../../../docs/configuration/packet-22"),
    include_bytes!("../../../docs/configuration/packet-23"),
    include_bytes!("../../../docs/configuration/packet-24"),
    include_bytes!("../../../docs/configuration/packet-25"),
    include_bytes!("../../../docs/configuration/packet-26"),
    include_bytes!("../../../docs/configuration/packet-27"),
    include_bytes!("../../../docs/configuration/packet-28"),
    include_bytes!("../../../docs/configuration/packet-29"),
];

pub fn configuration_fixture_stream() -> &'static [&'static [u8]] {
    &CONFIGURATION_FIXTURES
}

/// Settings for one accepted connection.
#[derive(Debug, Clone, Copy)]
pub struct SessionSettings {
    pub max_frame: usize,
    /// Period between clientbound Keep Alive packets; the oracle uses 1 s.
    pub keep_alive_interval: Duration,
}

impl SessionSettings {
    pub const fn new(max_frame: usize) -> Self {
        Self {
            max_frame,
            keep_alive_interval: DEFAULT_KEEP_ALIVE_INTERVAL,
        }
    }
}

/// Run the Configuration stage on a connection that has acknowledged Login.
///
/// Mirrors `statecoroutines::configuration`: sends the prebuilt fixtures and
/// Finish Configuration, parses Client Information, ignores other serverbound
/// packets and returns after a valid Finish Configuration acknowledgement.
/// `pending` holds frames already decoded from the same reads as the Login
/// acknowledgement; frames after the Finish acknowledgement are returned so
/// Play can process them.
pub async fn run_configuration(
    socket: &mut TcpStream,
    decoder: &mut FrameDecoder,
    connection: &mut Connection,
    pending: Vec<BytesMut>,
) -> Result<(Option<ClientInformation>, Vec<BytesMut>)> {
    if connection.state() != ConnectionState::Configuration {
        anyhow::bail!("configuration requires Configuration state");
    }
    for fixture in CONFIGURATION_FIXTURES {
        socket
            .write_all(&encode_frame(fixture)?)
            .await
            .context("write configuration fixture")?;
    }
    socket
        .write_all(&encode_frame(&connection.configuration_finish_packet()?)?)
        .await
        .context("write finish configuration")?;
    let mut client_information = None;
    let mut read_buf = [0_u8; 8192];
    let mut frames = pending;
    loop {
        let mut batch = frames.into_iter();
        while let Some(frame) = batch.next() {
            match peek_packet_id(&frame) {
                Ok(CONFIGURATION_SERVERBOUND_CLIENT_INFORMATION_PACKET_ID) => {
                    match connection.parse_client_information(&frame) {
                        Ok(info) => client_information = Some(info),
                        Err(error) => {
                            let reason = format!("Protocol desync (Client Information): {error}");
                            send_disconnect(socket, encode_configuration_disconnect(&reason)).await;
                            anyhow::bail!(reason);
                        }
                    }
                }
                Ok(CONFIGURATION_SERVERBOUND_FINISH_ACK_PACKET_ID) => {
                    if let Err(error) = connection.acknowledge_configuration(&frame) {
                        let reason =
                            format!("Protocol desync (Acknowledge Finish Configuration): {error}");
                        send_disconnect(socket, encode_configuration_disconnect(&reason)).await;
                        anyhow::bail!(reason);
                    }
                    return Ok((client_information, batch.collect()));
                }
                Ok(packet_id) => {
                    // The oracle ignores other serverbound Configuration packets
                    // (brand plugin message, known packs, ...).
                    tracing::trace!(packet_id, "ignored configuration packet");
                }
                Err(error) => {
                    let reason = format!("Protocol desync (Configuration): {error}");
                    send_disconnect(socket, encode_configuration_disconnect(&reason)).await;
                    anyhow::bail!(reason);
                }
            }
        }
        let count = socket
            .read(&mut read_buf)
            .await
            .context("read configuration packet")?;
        if count == 0 {
            anyhow::bail!("connection closed before configuration acknowledgement");
        }
        frames = decoder
            .push(&read_buf[..count])
            .context("decode configuration frame")?;
    }
}

/// Minimal Play session: Keep Alive scheduling and validation.
///
/// Mirrors `keepalive_loop` from the oracle: a challenge is sent every
/// interval, replies must match one of the pending challenges, and the client
/// is disconnected after [`calcite_net::KEEP_ALIVE_MAX_MISSED_TICKS`] ticks
/// without a reply. Other Play packets are not implemented yet and are
/// ignored, like packets without subscribers in the oracle's router. Returns
/// `Ok(())` when the client closes the connection.
pub async fn run_play(
    socket: &mut TcpStream,
    decoder: &mut FrameDecoder,
    connection: &Connection,
    keep_alive_interval: Duration,
    pending: Vec<BytesMut>,
) -> Result<()> {
    if connection.state() != ConnectionState::Play {
        anyhow::bail!("play session requires Play state");
    }
    let mut tracker = KeepAliveTracker::new();
    let mut payloads = KeepAlivePayloads::default();
    let start = tokio::time::Instant::now() + keep_alive_interval;
    let mut ticker = tokio::time::interval_at(start, keep_alive_interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut read_buf = [0_u8; 8192];
    let mut frames = pending;
    loop {
        for frame in frames {
            match peek_packet_id(&frame) {
                Ok(PLAY_SERVERBOUND_KEEP_ALIVE_PACKET_ID) => {
                    if let Err(error) =
                        connection.handle_play_keep_alive_reply(&frame, &mut tracker)
                    {
                        let reason = format!("Protocol desync (Serverbound Keep Alive): {error}");
                        send_disconnect(socket, encode_play_disconnect(&reason)).await;
                        anyhow::bail!(reason);
                    }
                }
                Ok(PLAY_SERVERBOUND_CLIENT_TICK_END_PACKET_ID) => {}
                Ok(packet_id) => {
                    tracing::trace!(packet_id, "play packet without a handler");
                }
                Err(error) => {
                    let reason = format!("Protocol desync (Play): {error}");
                    send_disconnect(socket, encode_play_disconnect(&reason)).await;
                    anyhow::bail!(reason);
                }
            }
        }
        frames = Vec::new();
        tokio::select! {
            read = socket.read(&mut read_buf) => {
                let count = read.context("read play packet")?;
                if count == 0 {
                    return Ok(());
                }
                frames = decoder
                    .push(&read_buf[..count])
                    .context("decode play frame")?;
            }
            _ = ticker.tick() => {
                let payload = match tracker.issue(payloads.next_payload()) {
                    Ok(payload) => payload,
                    Err(_) => {
                        send_disconnect(socket, encode_play_disconnect(KEEP_ALIVE_TIMEOUT_REASON))
                            .await;
                        anyhow::bail!("keep alive timeout");
                    }
                };
                socket
                    .write_all(&encode_frame(&encode_play_clientbound_keep_alive(payload))?)
                    .await
                    .context("write clientbound keep alive")?;
            }
        }
    }
}

/// Best-effort delivery of a disconnect packet before the socket is dropped;
/// write failures are only logged because the connection is closing anyway.
async fn send_disconnect(
    socket: &mut TcpStream,
    packet: Result<Vec<u8>, calcite_protocol::ProtocolError>,
) {
    let result = match packet.and_then(|packet| encode_frame(&packet)) {
        Ok(frame) => socket.write_all(&frame).await.map_err(anyhow::Error::from),
        Err(error) => Err(error.into()),
    };
    if let Err(error) = result {
        tracing::debug!(%error, "disconnect reason could not be sent");
    }
}

pub async fn serve(listener: TcpListener, settings: SessionSettings) -> Result<()> {
    loop {
        let (socket, peer) = listener.accept().await.context("accept TCP connection")?;
        tracing::debug!(%peer, "accepted connection");
        tokio::spawn(async move {
            if let Err(error) = handle_session(socket, settings).await {
                tracing::debug!(%peer, %error, "connection closed with error");
            }
        });
    }
}

#[cfg(test)]
async fn handle_connection(socket: TcpStream, max_frame: usize) -> Result<()> {
    handle_session(socket, SessionSettings::new(max_frame)).await
}

async fn handle_session(mut socket: TcpStream, settings: SessionSettings) -> Result<()> {
    let mut decoder = FrameDecoder::new(settings.max_frame);
    let mut read_buf = [0_u8; 8192];
    let mut connection = Connection::new();
    let mut handshake_done = false;
    loop {
        let count = socket
            .read(&mut read_buf)
            .await
            .context("read TCP connection")?;
        if count == 0 {
            return Ok(());
        }
        let mut batch = decoder
            .push(&read_buf[..count])
            .context("decode Minecraft frame")?
            .into_iter();
        while let Some(frame) = batch.next() {
            if !handshake_done {
                let handshake = parse_handshake(&frame).context("parse handshake")?;
                match handshake.intent {
                    HandshakeIntent::Status => connection
                        .transition(ConnectionState::Status)
                        .context("enter status state")?,
                    HandshakeIntent::Login => connection
                        .transition(ConnectionState::Login)
                        .context("enter login state")?,
                    HandshakeIntent::Transfer => anyhow::bail!("transfer is not implemented"),
                }
                handshake_done = true;
                continue;
            }
            if connection.state() == ConnectionState::Login {
                let login = connection
                    .parse_login_start(&frame)
                    .context("parse login start")?;
                socket
                    .write_all(&encode_frame(&connection.login_success(&login)?)?)
                    .await
                    .context("write login success")?;
                let mut frames: Vec<BytesMut> = batch.collect();
                while frames.is_empty() {
                    let count = socket
                        .read(&mut read_buf)
                        .await
                        .context("read login acknowledgement")?;
                    if count == 0 {
                        anyhow::bail!("connection closed before login acknowledgement");
                    }
                    frames = decoder
                        .push(&read_buf[..count])
                        .context("decode login acknowledgement")?;
                }
                let pending = frames.split_off(1);
                connection
                    .acknowledge_login(&frames[0])
                    .context("acknowledge login")?;
                let (client_information, pending) =
                    run_configuration(&mut socket, &mut decoder, &mut connection, pending).await?;
                tracing::debug!(
                    player = %login.name,
                    view_distance = client_information
                        .as_ref()
                        .map(ClientInformation::effective_view_distance),
                    "entered play"
                );
                return run_play(
                    &mut socket,
                    &mut decoder,
                    &connection,
                    settings.keep_alive_interval,
                    pending,
                )
                .await;
            }
            if connection.state() != ConnectionState::Status {
                anyhow::bail!("unexpected connection state");
            }
            let response = match frame.first().copied() {
                Some(0x00) if frame.len() == 1 => status_response(),
                Some(0x01) if frame.len() == 9 => frame.to_vec(),
                _ => anyhow::bail!("malformed status packet"),
            };
            socket
                .write_all(&encode_frame(&response).context("encode response frame")?)
                .await
                .context("write status response")?;
            if response.first() == Some(&0x01) {
                return Ok(());
            }
        }
    }
}

fn status_response() -> Vec<u8> {
    let body = r#"{"version":{"name":"26.1.2","protocol":775},"players":{"max":0,"online":0,"sample":[]},"description":{"text":"An ActualKlasterKraft Server"},"enforcesSecureChat":false}"#;
    let mut payload = vec![0x00];
    write_var_i32(body.len() as i32, &mut payload);
    payload.extend_from_slice(body.as_bytes());
    payload
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let listener = TcpListener::bind(("0.0.0.0", 25565))
        .await
        .context("bind Minecraft server port 25565")?;
    tracing::info!(
        protocol = calcite_protocol_version(),
        address = ?listener.local_addr()?,
        "Calcite Rust foundation starting"
    );
    serve(listener, SessionSettings::new(DEFAULT_MAX_FRAME)).await
}

fn calcite_protocol_version() -> i32 {
    775
}

#[cfg(test)]
mod tests {
    use super::*;
    use calcite_protocol::encode_frame;
    use tokio::time::{Duration, timeout};

    #[tokio::test]
    async fn accepts_loopback_connection_and_reads_frame() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let mut client = TcpStream::connect(address).await.unwrap();
        let (socket, _) = listener.accept().await.unwrap();
        let frame = encode_frame(&handshake()).unwrap();
        client.write_all(&frame[..2]).await.unwrap();
        client.write_all(&frame[2..]).await.unwrap();
        client.shutdown().await.unwrap();
        timeout(Duration::from_secs(1), handle_connection(socket, 1024))
            .await
            .unwrap()
            .unwrap();
    }

    fn handshake() -> Vec<u8> {
        let mut payload = Vec::new();
        write_var_i32(0, &mut payload);
        write_var_i32(775, &mut payload);
        calcite_protocol::write_string("localhost", &mut payload).unwrap();
        payload.extend_from_slice(&25565u16.to_be_bytes());
        write_var_i32(1, &mut payload);
        payload
    }

    #[tokio::test]
    async fn status_ping_returns_json_and_pong() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            handle_connection(socket, 2048).await.unwrap();
        });
        let mut client = TcpStream::connect(address).await.unwrap();
        client
            .write_all(&encode_frame(&handshake()).unwrap())
            .await
            .unwrap();
        client
            .write_all(&encode_frame(&[0]).unwrap())
            .await
            .unwrap();
        let mut buf = [0_u8; 2048];
        let n = client.read(&mut buf).await.unwrap();
        let status = FrameDecoder::new(2048)
            .push(&buf[..n])
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(status[0], 0);
        client
            .write_all(&encode_frame(&[1, 1, 2, 3, 4, 5, 6, 7, 8]).unwrap())
            .await
            .unwrap();
        let n = client.read(&mut buf).await.unwrap();
        let pong = FrameDecoder::new(2048)
            .push(&buf[..n])
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(&pong[..], &[1, 1, 2, 3, 4, 5, 6, 7, 8]);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn malformed_handshake_closes_connection() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            handle_connection(socket, 1024).await.is_err()
        });
        let mut client = TcpStream::connect(address).await.unwrap();
        client
            .write_all(&encode_frame(&[1]).unwrap())
            .await
            .unwrap();
        let mut buf = [0_u8; 1];
        assert_eq!(client.read(&mut buf).await.unwrap(), 0);
        assert!(server.await.unwrap());
    }

    #[tokio::test]
    async fn malformed_status_ack_packet_closes_connection() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            handle_connection(socket, 1024).await.is_err()
        });
        let mut client = TcpStream::connect(address).await.unwrap();
        client
            .write_all(&encode_frame(&handshake()).unwrap())
            .await
            .unwrap();
        client
            .write_all(&encode_frame(&[0x03]).unwrap())
            .await
            .unwrap();
        let mut buf = [0_u8; 1];
        assert_eq!(client.read(&mut buf).await.unwrap(), 0);
        assert!(server.await.unwrap());
    }

    const TEST_INTERVAL: Duration = Duration::from_millis(50);

    fn login_handshake() -> Vec<u8> {
        let mut payload = handshake();
        *payload.last_mut().unwrap() = 2;
        payload
    }

    fn login_start() -> Vec<u8> {
        let mut payload = vec![0x00, 0x05];
        payload.extend_from_slice(b"Steve");
        payload.extend_from_slice(&[0; 16]);
        payload
    }

    fn client_information() -> Vec<u8> {
        vec![
            0x00, 0x05, b'e', b'n', b'_', b'u', b's', 12, 0, 1, 0x7f, 1, 0, 1, 0,
        ]
    }

    struct TestClient {
        stream: TcpStream,
        decoder: FrameDecoder,
        frames: std::collections::VecDeque<BytesMut>,
    }

    impl TestClient {
        async fn send(&mut self, payload: &[u8]) {
            self.stream
                .write_all(&encode_frame(payload).unwrap())
                .await
                .unwrap();
        }

        /// Next frame from the server, or `None` on EOF.
        async fn recv(&mut self) -> Option<BytesMut> {
            let mut buf = [0_u8; 8192];
            loop {
                if let Some(frame) = self.frames.pop_front() {
                    return Some(frame);
                }
                let n = timeout(Duration::from_secs(2), self.stream.read(&mut buf))
                    .await
                    .expect("server response timed out")
                    .unwrap();
                if n == 0 {
                    return None;
                }
                self.frames.extend(self.decoder.push(&buf[..n]).unwrap());
            }
        }

        async fn recv_with_id(&mut self, id: u8) -> BytesMut {
            loop {
                let frame = self.recv().await.expect("connection closed");
                if frame.first() == Some(&id) {
                    return frame;
                }
            }
        }
    }

    async fn spawn_session() -> (TestClient, tokio::task::JoinHandle<Result<()>>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let settings = SessionSettings {
                max_frame: DEFAULT_MAX_FRAME,
                keep_alive_interval: TEST_INTERVAL,
            };
            handle_session(socket, settings).await
        });
        let client = TestClient {
            stream: TcpStream::connect(address).await.unwrap(),
            decoder: FrameDecoder::new(DEFAULT_MAX_FRAME),
            frames: Default::default(),
        };
        (client, server)
    }

    /// Drive a client into Play; returns after the Finish ACK was sent.
    async fn enter_play(client: &mut TestClient) {
        client.send(&login_handshake()).await;
        client.send(&login_start()).await;
        assert_eq!(client.recv().await.unwrap()[0], 0x02);
        client.send(&[0x03]).await;
        for fixture in configuration_fixture_stream() {
            let frame = client.recv().await.unwrap();
            let mut expected = BytesMut::from(&encode_frame(fixture).unwrap()[..]);
            let expected = decode_frame(&mut expected).unwrap().unwrap();
            assert_eq!(frame, expected);
        }
        assert_eq!(client.recv().await.unwrap().as_ref(), &[0x03]);
        client.send(&client_information()).await;
        // Brand plugin message: ignored like in the oracle.
        client.send(&[0x02, 0x00]).await;
        client.send(&[0x03]).await;
    }

    fn keep_alive_reply(clientbound: &[u8]) -> Vec<u8> {
        assert_eq!(clientbound.len(), 9);
        assert_eq!(clientbound[0], 0x2c);
        let mut reply = vec![0x1c];
        reply.extend_from_slice(&clientbound[1..]);
        reply
    }

    fn nbt_reason(disconnect: &[u8]) -> String {
        assert_eq!(disconnect[1], 0x08);
        let len = u16::from_be_bytes([disconnect[2], disconnect[3]]) as usize;
        assert_eq!(disconnect.len(), 4 + len);
        String::from_utf8(disconnect[4..].to_vec()).unwrap()
    }

    use calcite_protocol::decode_frame;

    #[tokio::test]
    async fn play_session_sends_keep_alive_and_accepts_replies() {
        let (mut client, server) = spawn_session().await;
        enter_play(&mut client).await;
        for _ in 0..3 {
            let keep_alive = client.recv_with_id(0x2c).await;
            client.send(&keep_alive_reply(&keep_alive)).await;
            // Client Tick End and an unhandled packet must not break the loop.
            client.send(&[0x0d]).await;
            client.send(&[0x7f, 1, 2]).await;
        }
        client.stream.shutdown().await.unwrap();
        drop(client);
        server.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn play_session_accepts_login_and_configuration_in_one_write() {
        let (mut client, server) = spawn_session().await;
        let mut burst = Vec::new();
        for payload in [login_handshake(), login_start(), vec![0x03]] {
            burst.extend_from_slice(&encode_frame(&payload).unwrap());
        }
        client.stream.write_all(&burst).await.unwrap();
        let mut seen = 0;
        while seen < 1 + configuration_fixture_stream().len() + 1 {
            client.recv().await.unwrap();
            seen += 1;
        }
        let mut burst = Vec::new();
        for payload in [client_information(), vec![0x03]] {
            burst.extend_from_slice(&encode_frame(&payload).unwrap());
        }
        client.stream.write_all(&burst).await.unwrap();
        let keep_alive = client.recv_with_id(0x2c).await;
        client.send(&keep_alive_reply(&keep_alive)).await;
        client.stream.shutdown().await.unwrap();
        drop(client);
        server.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn play_session_disconnects_on_mismatched_keep_alive() {
        let (mut client, server) = spawn_session().await;
        enter_play(&mut client).await;
        let keep_alive = client.recv_with_id(0x2c).await;
        let mut reply = keep_alive_reply(&keep_alive);
        reply[8] ^= 0xff;
        client.send(&reply).await;
        let disconnect = client.recv_with_id(0x20).await;
        assert!(nbt_reason(&disconnect).contains("Serverbound Keep Alive"));
        assert!(server.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn play_session_disconnects_on_malformed_keep_alive() {
        let (mut client, server) = spawn_session().await;
        enter_play(&mut client).await;
        client.recv_with_id(0x2c).await;
        client.send(&[0x1c, 0, 0]).await;
        let disconnect = client.recv_with_id(0x20).await;
        assert!(nbt_reason(&disconnect).contains("Serverbound Keep Alive"));
        assert!(server.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn play_session_times_out_without_replies() {
        let (mut client, server) = spawn_session().await;
        enter_play(&mut client).await;
        let mut keep_alives = 0;
        let disconnect = loop {
            let frame = client.recv().await.expect("connection closed");
            match frame[0] {
                0x2c => keep_alives += 1,
                0x20 => break frame,
                _ => {}
            }
        };
        assert_eq!(keep_alives, calcite_net::KEEP_ALIVE_MAX_MISSED_TICKS);
        assert_eq!(nbt_reason(&disconnect), KEEP_ALIVE_TIMEOUT_REASON);
        assert!(client.recv().await.is_none());
        assert!(server.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn malformed_client_information_disconnects_in_configuration() {
        let (mut client, server) = spawn_session().await;
        client.send(&login_handshake()).await;
        client.send(&login_start()).await;
        client.recv().await.unwrap();
        client.send(&[0x03]).await;
        for _ in 0..=configuration_fixture_stream().len() {
            client.recv().await.unwrap();
        }
        let mut info = client_information();
        info.push(0);
        client.send(&info).await;
        let disconnect = client.recv().await.unwrap();
        assert_eq!(disconnect[0], 0x02);
        assert!(nbt_reason(&disconnect).contains("Client Information"));
        assert!(server.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn malformed_finish_acknowledgement_disconnects_in_configuration() {
        let (mut client, server) = spawn_session().await;
        client.send(&login_handshake()).await;
        client.send(&login_start()).await;
        client.recv().await.unwrap();
        client.send(&[0x03]).await;
        for _ in 0..=configuration_fixture_stream().len() {
            client.recv().await.unwrap();
        }
        client.send(&[0x03, 0x00]).await;
        let disconnect = client.recv().await.unwrap();
        assert_eq!(disconnect[0], 0x02);
        assert!(nbt_reason(&disconnect).contains("Acknowledge Finish Configuration"));
        assert!(server.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn transfer_handshake_is_rejected() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            handle_connection(socket, 1024).await.is_err()
        });
        let mut client = TcpStream::connect(address).await.unwrap();
        let mut login_handshake = handshake();
        *login_handshake.last_mut().unwrap() = 3;
        client
            .write_all(&encode_frame(&login_handshake).unwrap())
            .await
            .unwrap();
        let mut buf = [0_u8; 1];
        assert_eq!(client.read(&mut buf).await.unwrap(), 0);
        assert!(server.await.unwrap());
    }
}
