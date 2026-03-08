//! Wrappers around ffprobe and ffmpeg for media inspection and rendering.
//!
//! All functions shell out to the system `ffprobe`/`ffmpeg` binaries
//! (configurable via `FFPROBE_BIN` / `FFMPEG_BIN` env vars).

pub mod ffmpeg;
pub mod ffprobe;

/// Resolve the ffprobe binary path from env or default.
pub fn ffprobe_bin() -> String {
    std::env::var("FFPROBE_BIN").unwrap_or_else(|_| "ffprobe".into())
}

/// Resolve the ffmpeg binary path from env or default.
pub fn ffmpeg_bin() -> String {
    std::env::var("FFMPEG_BIN").unwrap_or_else(|_| "ffmpeg".into())
}
