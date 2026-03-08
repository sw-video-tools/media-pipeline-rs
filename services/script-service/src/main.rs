#[tokio::main]
async fn main() -> anyhow::Result<()> {
    telemetry::init();
    tracing::info!(service = "script-service", "service stub started");
    Ok(())
}
