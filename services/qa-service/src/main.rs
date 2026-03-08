#[tokio::main]
async fn main() -> anyhow::Result<()> {
    telemetry::init();
    tracing::info!(service = "qa-service", "service stub started");
    Ok(())
}
