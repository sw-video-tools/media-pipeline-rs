use anyhow::Result;
use pipeline_types::PipelineJob;

#[derive(Debug, Default)]
pub struct JobQueue;

impl JobQueue {
    pub fn new() -> Self {
        Self
    }

    pub async fn enqueue(&self, job: &PipelineJob) -> Result<()> {
        let _ = job;
        Ok(())
    }
}
