use anyhow::Result;
use pipeline_types::PipelineJob;

#[derive(Debug, Default)]
pub struct ArtifactStore;

impl ArtifactStore {
    pub fn new() -> Self {
        Self
    }

    pub async fn save_job(&self, job: &PipelineJob) -> Result<()> {
        let _ = job;
        Ok(())
    }
}
