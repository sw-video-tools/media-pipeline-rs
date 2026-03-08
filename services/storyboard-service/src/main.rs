#[tokio::main]
async fn main() -> anyhow::Result<()> {
    telemetry::init();
    tracing::info!(service = "storyboard-service", "service stub started");
    Ok(())
}
