use anyhow::{Context, Result};
use calcite_net::FrameDecoder;
use tokio::io::AsyncReadExt;
use tokio::net::{TcpListener, TcpStream};

const DEFAULT_MAX_FRAME: usize = 2 * 1024 * 1024;

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
    loop {
        let count = socket
            .read(&mut read_buf)
            .await
            .context("read TCP connection")?;
        if count == 0 {
            return Ok(());
        }
        decoder
            .push(&read_buf[..count])
            .context("decode Minecraft frame")?;
    }
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
    use tokio::io::AsyncWriteExt;
    use tokio::time::{Duration, timeout};

    #[tokio::test]
    async fn accepts_loopback_connection_and_reads_frame() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let mut client = TcpStream::connect(address).await.unwrap();
        let (socket, _) = listener.accept().await.unwrap();
        let frame = encode_frame(b"handshake").unwrap();
        client.write_all(&frame[..2]).await.unwrap();
        client.write_all(&frame[2..]).await.unwrap();
        client.shutdown().await.unwrap();
        timeout(Duration::from_secs(1), handle_connection(socket, 1024))
            .await
            .unwrap()
            .unwrap();
    }
}
