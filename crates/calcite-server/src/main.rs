use anyhow::{Context, Result};
use calcite_net::{Connection, ConnectionState, FrameDecoder};
use calcite_protocol::{HandshakeIntent, encode_frame, parse_handshake, write_var_i32};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const DEFAULT_MAX_FRAME: usize = 2 * 1024 * 1024;

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

pub async fn run_configuration(
    mut socket: TcpStream,
    connection: Connection,
    max_frame: usize,
) -> Result<Connection> {
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
    let mut decoder = FrameDecoder::new(max_frame);
    let mut read_buf = [0_u8; 1024];
    loop {
        let count = socket
            .read(&mut read_buf)
            .await
            .context("read configuration acknowledgement")?;
        if count == 0 {
            anyhow::bail!("connection closed before configuration acknowledgement");
        }
        if let Some(frame) = decoder.push(&read_buf[..count])?.first() {
            connection
                .validate_configuration_ack(frame)
                .map_err(|error| anyhow::anyhow!(error))?;
            return Ok(connection);
        }
    }
}

pub async fn serve(listener: TcpListener, max_frame: usize) -> Result<()> {
    loop {
        let (socket, peer) = listener.accept().await.context("accept TCP connection")?;
        tracing::debug!(%peer, "accepted connection");
        tokio::spawn(async move {
            if let Err(error) = handle_connection(socket, max_frame).await {
                tracing::debug!(%peer, %error, "connection closed with error");
            }
        });
    }
}

async fn handle_connection(mut socket: TcpStream, max_frame: usize) -> Result<()> {
    let mut decoder = FrameDecoder::new(max_frame);
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
        for frame in decoder
            .push(&read_buf[..count])
            .context("decode Minecraft frame")?
        {
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
                loop {
                    let count = socket
                        .read(&mut read_buf)
                        .await
                        .context("read login acknowledgement")?;
                    if count == 0 {
                        anyhow::bail!("connection closed before login acknowledgement");
                    }
                    let frames = decoder
                        .push(&read_buf[..count])
                        .context("decode login acknowledgement")?;
                    if let Some(ack) = frames.first() {
                        connection
                            .acknowledge_login(ack)
                            .context("acknowledge login")?;
                        break;
                    }
                }
                return run_configuration(socket, connection, max_frame)
                    .await
                    .map(|_| ());
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
    serve(listener, DEFAULT_MAX_FRAME).await
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
