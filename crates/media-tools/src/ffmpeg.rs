//! ffmpeg wrapper for media rendering and conversion.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use tracing::info;

/// Check that ffmpeg is available and return its version string.
pub fn version() -> Result<String> {
    let out = Command::new(crate::ffmpeg_bin())
        .arg("-version")
        .output()
        .context("failed to launch ffmpeg")?;
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Concatenate audio files using the ffmpeg concat demuxer.
/// `inputs` must be a list of paths to audio files.
pub fn concat_audio(inputs: &[&Path], output: &Path) -> Result<()> {
    if inputs.is_empty() {
        bail!("no input files provided");
    }

    let list_content: String = inputs
        .iter()
        .map(|p| format!("file '{}'\n", p.display()))
        .collect();

    let list_path = output.with_extension("concat.txt");
    std::fs::write(&list_path, &list_content).context("failed to write concat list")?;

    let status = Command::new(crate::ffmpeg_bin())
        .args(["-y", "-f", "concat", "-safe", "0", "-i"])
        .arg(&list_path)
        .args(["-c", "copy"])
        .arg(output)
        .status()
        .context("failed to run ffmpeg concat")?;

    // Clean up temp list
    let _ = std::fs::remove_file(&list_path);

    if !status.success() {
        bail!("ffmpeg concat failed with exit code {:?}", status.code());
    }

    info!(output = %output.display(), "audio concatenated");
    Ok(())
}

/// Render a simple slideshow video from images with an audio track.
pub fn render_slideshow(images: &[&Path], audio: &Path, output: &Path, fps: u32) -> Result<()> {
    if images.is_empty() {
        bail!("no images provided");
    }

    let list_content: String = images
        .iter()
        .map(|p| format!("file '{}'\nduration 5\n", p.display()))
        .collect();

    let list_path = output.with_extension("images.txt");
    std::fs::write(&list_path, &list_content).context("failed to write image list")?;

    let fps_str = fps.to_string();
    let status = Command::new(crate::ffmpeg_bin())
        .args(["-y", "-f", "concat", "-safe", "0", "-i"])
        .arg(&list_path)
        .args(["-i"])
        .arg(audio)
        .args([
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-r",
            &fps_str,
            "-c:a",
            "aac",
            "-shortest",
        ])
        .arg(output)
        .status()
        .context("failed to run ffmpeg slideshow render")?;

    let _ = std::fs::remove_file(&list_path);

    if !status.success() {
        bail!(
            "ffmpeg slideshow render failed with exit code {:?}",
            status.code()
        );
    }

    info!(output = %output.display(), "slideshow rendered");
    Ok(())
}

/// Measure integrated loudness (LUFS) of an audio file.
/// Returns the integrated loudness value.
pub fn measure_loudness(path: &Path) -> Result<f64> {
    let output = Command::new(crate::ffmpeg_bin())
        .args(["-i"])
        .arg(path)
        .args(["-af", "loudnorm=print_format=json", "-f", "null", "-"])
        .output()
        .context("failed to run ffmpeg loudnorm")?;

    let stderr = String::from_utf8_lossy(&output.stderr);

    // Parse the loudnorm JSON from stderr
    if let Some(start) = stderr.find("\"input_i\"") {
        // Extract the value after "input_i" : "
        let after = &stderr[start..];
        if let Some(val_start) = after.find(": \"") {
            let val_str = &after[val_start + 3..];
            if let Some(val_end) = val_str.find('"')
                && let Ok(lufs) = val_str[..val_end].parse::<f64>()
            {
                return Ok(lufs);
            }
        }
    }

    bail!("could not parse loudness from ffmpeg output")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ffmpeg_version_runs() {
        if which_exists("ffmpeg") {
            let ver = version().unwrap();
            assert!(ver.contains("ffmpeg"));
        }
    }

    #[test]
    fn concat_rejects_empty_input() {
        let result = concat_audio(&[], Path::new("/tmp/out.mp3"));
        assert!(result.is_err());
    }

    #[test]
    fn slideshow_rejects_empty_images() {
        let result = render_slideshow(
            &[],
            Path::new("/tmp/audio.mp3"),
            Path::new("/tmp/out.mp4"),
            30,
        );
        assert!(result.is_err());
    }

    fn which_exists(name: &str) -> bool {
        Command::new("which")
            .arg(name)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}
