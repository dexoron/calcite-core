use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    tracing::info!(
        protocol = calcite_protocol_version(),
        "Calcite Rust foundation starting"
    );
    Ok(())
}

fn calcite_protocol_version() -> i32 {
    775
}
