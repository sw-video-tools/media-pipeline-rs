#[tokio::main]
async fn main() -> anyhow::Result<()> {
    telemetry::init();
    tracing::info!(service = "preview-service", "service stub started");
    Ok(())
}
