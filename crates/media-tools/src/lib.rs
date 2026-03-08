use anyhow::{Context, Result};
use std::process::Command;

pub fn ffprobe_version() -> Result<String> {
    let out = Command::new("ffprobe")
        .arg("-version")
        .output()
        .context("failed to launch ffprobe")?;
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}
