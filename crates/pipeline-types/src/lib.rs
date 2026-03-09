use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProducerInfo {
    pub service_name: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ValidationState {
    Pending,
    Passed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactEnvelope<T> {
    pub artifact_id: String,
    pub artifact_type: String,
    pub version: u32,
    pub created_at: DateTime<Utc>,
    pub produced_by: ProducerInfo,
    pub input_refs: Vec<String>,
    pub evidence_refs: Vec<String>,
    pub validation: ValidationState,
    pub content_hash: String,
    pub payload: T,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdeaBrief {
    pub title: String,
    pub audience: String,
    pub objective: String,
    pub target_duration_seconds: u32,
    pub tone: String,
    pub must_include: Vec<String>,
    pub must_avoid: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineJobRequest {
    pub title: String,
    pub idea: String,
    pub audience: String,
    pub target_duration_seconds: u32,
    pub tone: String,
    pub must_include: Vec<String>,
    pub must_avoid: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum JobStatus {
    Queued,
    Running,
    WaitingForReview,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Stage {
    Planning,
    Research,
    Script,
    Storyboard,
    VisualGeneration,
    MusicGeneration,
    Tts,
    AsrValidation,
    Captions,
    RenderPreview,
    QaPreview,
    RenderFinal,
    QaFinal,
    Complete,
    Failed,
}

impl fmt::Display for Stage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Planning => write!(f, "Planning"),
            Self::Research => write!(f, "Research"),
            Self::Script => write!(f, "Script"),
            Self::Storyboard => write!(f, "Storyboard"),
            Self::VisualGeneration => write!(f, "VisualGeneration"),
            Self::MusicGeneration => write!(f, "MusicGeneration"),
            Self::Tts => write!(f, "Tts"),
            Self::AsrValidation => write!(f, "AsrValidation"),
            Self::Captions => write!(f, "Captions"),
            Self::RenderPreview => write!(f, "RenderPreview"),
            Self::QaPreview => write!(f, "QaPreview"),
            Self::RenderFinal => write!(f, "RenderFinal"),
            Self::QaFinal => write!(f, "QaFinal"),
            Self::Complete => write!(f, "Complete"),
            Self::Failed => write!(f, "Failed"),
        }
    }
}

impl Stage {
    /// Return the next stage in the MVP pipeline, or None if complete/failed.
    pub fn next_mvp(&self) -> Option<Stage> {
        match self {
            Self::Planning => Some(Self::Research),
            Self::Research => Some(Self::Script),
            Self::Script => Some(Self::Tts),
            Self::Tts => Some(Self::AsrValidation),
            Self::AsrValidation => Some(Self::Captions),
            Self::Captions => Some(Self::RenderFinal),
            Self::RenderFinal => Some(Self::QaFinal),
            Self::QaFinal => Some(Self::Complete),
            Self::Complete | Self::Failed => None,
            // Non-MVP stages skip to next MVP stage
            Self::Storyboard | Self::VisualGeneration | Self::MusicGeneration => Some(Self::Tts),
            Self::RenderPreview | Self::QaPreview => Some(Self::RenderFinal),
        }
    }
}

impl fmt::Display for JobStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Queued => write!(f, "Queued"),
            Self::Running => write!(f, "Running"),
            Self::WaitingForReview => write!(f, "WaitingForReview"),
            Self::Completed => write!(f, "Completed"),
            Self::Failed => write!(f, "Failed"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineJob {
    pub job_id: String,
    pub submitted_at: DateTime<Utc>,
    pub status: JobStatus,
    pub current_stage: Stage,
    pub request: PipelineJobRequest,
}

/// A verified fact from research, with source attribution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchFact {
    pub claim: String,
    pub source: String,
    pub confidence: f32,
}

/// Output of the research stage: gathered facts for script writing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchPacket {
    pub job_id: String,
    pub query: String,
    pub facts: Vec<ResearchFact>,
    pub summary: String,
}

/// A planned segment within the project plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannedSegment {
    pub segment_number: u32,
    pub title: String,
    pub duration_seconds: u32,
    pub description: String,
    pub visual_style: String,
}

/// Output of the planning stage: a structured project plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectPlan {
    pub job_id: String,
    pub title: String,
    pub synopsis: String,
    pub total_duration_seconds: u32,
    pub segments: Vec<PlannedSegment>,
    pub research_queries: Vec<String>,
    pub narration_tone: String,
}

/// A single narration segment with timing and visual cues.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptSegment {
    pub segment_number: u32,
    pub title: String,
    pub narration_text: String,
    pub estimated_duration_seconds: u32,
    pub visual_notes: String,
}

/// Output of the script stage: full narration script with segments.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NarrationScript {
    pub job_id: String,
    pub title: String,
    pub total_duration_seconds: u32,
    pub segments: Vec<ScriptSegment>,
}
