#[tokio::main]
async fn main() -> anyhow::Result<()> {
    telemetry::init();
    tracing::info!(service = "orchestrator-service", "service stub started");
    Ok(())
}
