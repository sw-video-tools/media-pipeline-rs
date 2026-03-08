#[tokio::main]
async fn main() -> anyhow::Result<()> {
    telemetry::init();
    tracing::info!(service = "tts-service", "service stub started");
    Ok(())
}
