#[tokio::main]
async fn main() -> anyhow::Result<()> {
    telemetry::init();
    tracing::info!(service = "asr-validation-service", "service stub started");
    Ok(())
}
