#[tokio::main]
async fn main() -> anyhow::Result<()> {
    telemetry::init();
    tracing::info!(service = "captions-service", "service stub started");
    Ok(())
}
