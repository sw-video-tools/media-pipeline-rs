//! ffprobe wrapper for media file inspection.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tracing::debug;

/// Metadata extracted from a media file via ffprobe.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaInfo {
    pub duration_seconds: f64,
    pub format_name: String,
    pub streams: Vec<StreamInfo>,
}

/// Per-stream metadata from ffprobe.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamInfo {
    pub index: u32,
    pub codec_type: String,
    pub codec_name: String,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    #[serde(default)]
    pub sample_rate: Option<String>,
    #[serde(default)]
    pub channels: Option<u32>,
}

/// Check that ffprobe is available and return its version string.
pub fn version() -> Result<String> {
    let out = Command::new(crate::ffprobe_bin())
        .arg("-version")
        .output()
        .context("failed to launch ffprobe")?;
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Probe a media file and return structured metadata.
pub fn probe(path: &Path) -> Result<MediaInfo> {
    if !path.exists() {
        bail!("file not found: {}", path.display());
    }

    let output = Command::new(crate::ffprobe_bin())
        .args([
            "-v",
            "quiet",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
        ])
        .arg(path)
        .output()
        .with_context(|| format!("failed to run ffprobe on {}", path.display()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("ffprobe failed: {stderr}");
    }

    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).context("failed to parse ffprobe JSON output")?;

    debug!("ffprobe raw output for {}", path.display());

    let duration_seconds = json
        .pointer("/format/duration")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0);

    let format_name = json
        .pointer("/format/format_name")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    let streams: Vec<StreamInfo> = json
        .get("streams")
        .and_then(|s| serde_json::from_value(s.clone()).ok())
        .unwrap_or_default();

    Ok(MediaInfo {
        duration_seconds,
        format_name,
        streams,
    })
}

/// Get the duration of a media file in seconds.
pub fn duration(path: &Path) -> Result<f64> {
    let info = probe(path)?;
    Ok(info.duration_seconds)
}

/// Check if a file has an audio stream.
pub fn has_audio(path: &Path) -> Result<bool> {
    let info = probe(path)?;
    Ok(info.streams.iter().any(|s| s.codec_type == "audio"))
}

/// Check if a file has a video stream.
pub fn has_video(path: &Path) -> Result<bool> {
    let info = probe(path)?;
    Ok(info.streams.iter().any(|s| s.codec_type == "video"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ffprobe_version_runs() {
        // Only run if ffprobe is installed
        if which_exists("ffprobe") {
            let ver = version().unwrap();
            assert!(ver.contains("ffprobe"));
        }
    }

    #[test]
    fn probe_missing_file_errors() {
        let result = probe(Path::new("/nonexistent/file.mp4"));
        assert!(result.is_err());
    }

    #[test]
    fn media_info_serializes() {
        let info = MediaInfo {
            duration_seconds: 10.5,
            format_name: "mp4".into(),
            streams: vec![StreamInfo {
                index: 0,
                codec_type: "video".into(),
                codec_name: "h264".into(),
                width: Some(1920),
                height: Some(1080),
                sample_rate: None,
                channels: None,
            }],
        };
        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains("1920"));
        assert!(json.contains("h264"));
    }

    fn which_exists(name: &str) -> bool {
        Command::new("which")
            .arg(name)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}
