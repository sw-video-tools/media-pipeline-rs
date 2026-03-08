#[tokio::main]
async fn main() -> anyhow::Result<()> {
    telemetry::init();
    tracing::info!(service = "visual-gen-service", "service stub started");
    Ok(())
}
