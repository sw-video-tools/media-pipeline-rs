#[tokio::main]
async fn main() -> anyhow::Result<()> {
    telemetry::init();
    tracing::info!(service = "render-service", "service stub started");
    Ok(())
}
