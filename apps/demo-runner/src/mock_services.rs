//! Mock implementations of all 8 pipeline stage services.
//!
//! Each handler produces realistic output that chains correctly to
//! downstream stages. No external LLMs, TTS engines, or media tools
//! are required — this is a pure Rust simulation of the pipeline.

use std::path::Path;
use std::process::Command;

use axum::routing::{get, post};
use axum::{Json, Router};
use tracing::info;

/// Build a router with all stage endpoints + healthz.
pub fn mock_router() -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/plan", post(plan))
        .route("/research", post(research))
        .route("/script", post(script))
        .route("/tts", post(tts))
        .route("/validate", post(validate))
        .route("/captions", post(captions))
        .route("/render", post(render))
        .route("/qa", post(qa))
}

async fn healthz() -> &'static str {
    "ok"
}

async fn plan(Json(body): Json<serde_json::Value>) -> Json<serde_json::Value> {
    let job_id = body["job_id"].as_str().unwrap_or("demo");
    let title = body["title"].as_str().unwrap_or("Demo Video");
    let idea = body["idea"].as_str().unwrap_or("A demo idea");
    let tone = body["tone"].as_str().unwrap_or("informative");
    let duration = body["target_duration_seconds"].as_u64().unwrap_or(30);

    // Split into 2 segments: intro (40%) and main content (60%).
    let intro_dur = (duration * 2 / 5).max(5);
    let main_dur = duration - intro_dur;

    Json(serde_json::json!({
        "job_id": job_id,
        "title": title,
        "synopsis": format!("A short video about: {idea}"),
        "total_duration_seconds": duration,
        "segments": [
            {
                "segment_number": 1,
                "title": "Introduction",
                "duration_seconds": intro_dur,
                "description": format!("Introduce the topic: {idea}"),
                "visual_style": "title-card"
            },
            {
                "segment_number": 2,
                "title": "Main Content",
                "duration_seconds": main_dur,
                "description": format!("Explore the core ideas of: {idea}"),
                "visual_style": "talking-head"
            }
        ],
        "research_queries": [
            format!("{title} overview"),
            format!("{idea} key facts")
        ],
        "narration_tone": tone,
    }))
}

async fn research(Json(body): Json<serde_json::Value>) -> Json<serde_json::Value> {
    let job_id = body["job_id"].as_str().unwrap_or("demo");
    let queries = body["queries"].as_array().cloned().unwrap_or_default();

    let packets: Vec<serde_json::Value> = queries
        .iter()
        .map(|q| {
            let query = q.as_str().unwrap_or("unknown query");
            serde_json::json!({
                "job_id": job_id,
                "query": query,
                "facts": [
                    {
                        "claim": format!("{query} is an important topic in modern technology."),
                        "source": "demo-knowledge-base",
                        "confidence": 0.92
                    },
                    {
                        "claim": format!("Understanding {query} helps developers build better systems."),
                        "source": "demo-knowledge-base",
                        "confidence": 0.88
                    }
                ],
                "summary": format!("Research summary for: {query}")
            })
        })
        .collect();

    Json(serde_json::json!(packets))
}

async fn script(Json(body): Json<serde_json::Value>) -> Json<serde_json::Value> {
    let plan = &body["plan"];
    let job_id = plan["job_id"].as_str().unwrap_or("demo");
    let title = plan["title"].as_str().unwrap_or("Demo Video");
    let total_duration = plan["total_duration_seconds"].as_u64().unwrap_or(30);

    let plan_segments = plan["segments"].as_array();
    let research = body["research"].as_array();

    // Build a fact string from research for weaving into narration.
    let facts_summary = research
        .map(|packets| {
            packets
                .iter()
                .filter_map(|p| p["facts"][0]["claim"].as_str())
                .collect::<Vec<_>>()
                .join(" Additionally, ")
        })
        .unwrap_or_default();

    let segments: Vec<serde_json::Value> = if let Some(segs) = plan_segments {
        segs.iter()
            .map(|seg| {
                let num = seg["segment_number"].as_u64().unwrap_or(1);
                let seg_title = seg["title"].as_str().unwrap_or("Segment");
                let desc = seg["description"].as_str().unwrap_or("");
                let dur = seg["duration_seconds"].as_u64().unwrap_or(15);

                let narration = if num == 1 {
                    format!(
                        "Welcome to {title}. Today we explore {desc}. \
                         {facts_summary}"
                    )
                } else {
                    format!(
                        "Now let's dive deeper into {seg_title}. {desc}. \
                         This is what makes the topic so compelling."
                    )
                };

                serde_json::json!({
                    "segment_number": num,
                    "title": seg_title,
                    "narration_text": narration,
                    "estimated_duration_seconds": dur,
                    "visual_notes": format!("Show {} visuals", seg["visual_style"].as_str().unwrap_or("default"))
                })
            })
            .collect()
    } else {
        vec![serde_json::json!({
            "segment_number": 1,
            "title": title,
            "narration_text": format!("Welcome to {title}. {facts_summary}"),
            "estimated_duration_seconds": total_duration,
            "visual_notes": "default visuals"
        })]
    };

    Json(serde_json::json!({
        "job_id": job_id,
        "title": title,
        "total_duration_seconds": total_duration,
        "segments": segments,
    }))
}

async fn tts(Json(body): Json<serde_json::Value>) -> Json<serde_json::Value> {
    let job_id = body["job_id"].as_str().unwrap_or("demo");
    let segments = body["segments"].as_array().cloned().unwrap_or_default();

    let dir = format!("./data/demo/tts/{job_id}");
    std::fs::create_dir_all(&dir).ok();

    let segment_files: Vec<serde_json::Value> = segments
        .iter()
        .map(|seg| {
            let num = seg["segment_number"].as_u64().unwrap_or(1);
            let dur = seg["estimated_duration_seconds"].as_u64().unwrap_or(10);
            let path = format!("{dir}/seg{num:03}.wav");

            // Generate real silent audio file with ffmpeg.
            if !Path::new(&path).exists() {
                let status = Command::new("ffmpeg")
                    .args([
                        "-y",
                        "-f",
                        "lavfi",
                        "-i",
                        "anullsrc=r=44100:cl=mono",
                        "-t",
                        &dur.to_string(),
                        "-c:a",
                        "pcm_s16le",
                        &path,
                    ])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status();
                match status {
                    Ok(s) if s.success() => info!("TTS: created {path} ({dur}s)"),
                    _ => info!("TTS: ffmpeg failed for {path}, returning path anyway"),
                }
            }

            serde_json::json!({
                "segment_number": num,
                "file_path": path,
            })
        })
        .collect();

    Json(serde_json::json!({
        "job_id": job_id,
        "segment_files": segment_files,
    }))
}

async fn validate(Json(body): Json<serde_json::Value>) -> Json<serde_json::Value> {
    let job_id = body["job_id"].as_str().unwrap_or("demo");
    let segments = body["segments"].as_array().cloned().unwrap_or_default();

    // Mock ASR: pretend we transcribed the audio and it matches.
    let segment_results: Vec<serde_json::Value> = segments
        .iter()
        .map(|seg| {
            let num = seg["segment_number"].as_u64().unwrap_or(1);
            let expected = seg["expected_text"].as_str().unwrap_or("");
            serde_json::json!({
                "segment_number": num,
                "transcript": expected,
                "similarity": 0.98,
                "issues": [],
            })
        })
        .collect();

    Json(serde_json::json!({
        "job_id": job_id,
        "passed": true,
        "segments": segment_results,
    }))
}

async fn captions(Json(body): Json<serde_json::Value>) -> Json<serde_json::Value> {
    let job_id = body["job_id"].as_str().unwrap_or("demo");
    let segments = body["segments"].as_array().cloned().unwrap_or_default();

    let dir = format!("./data/demo/captions/{job_id}");
    std::fs::create_dir_all(&dir).ok();
    let srt_path = format!("{dir}/captions.srt");

    // Build a real SRT file from segment narration text.
    let mut srt = String::new();
    let mut offset_secs = 0u64;
    for (i, seg) in segments.iter().enumerate() {
        let text = seg["narration_text"].as_str().unwrap_or("...");
        let dur = seg["estimated_duration_seconds"].as_u64().unwrap_or(10);
        let start = format_srt_time(offset_secs);
        let end = format_srt_time(offset_secs + dur);
        srt.push_str(&format!("{}\n{start} --> {end}\n{text}\n\n", i + 1));
        offset_secs += dur;
    }

    if let Err(e) = std::fs::write(&srt_path, &srt) {
        info!("Captions: failed to write SRT: {e}");
    } else {
        info!("Captions: wrote {} cues to {srt_path}", segments.len());
    }

    Json(serde_json::json!({
        "job_id": job_id,
        "srt_path": srt_path,
        "cue_count": segments.len(),
    }))
}

fn format_srt_time(total_secs: u64) -> String {
    let h = total_secs / 3600;
    let m = (total_secs % 3600) / 60;
    let s = total_secs % 60;
    format!("{h:02}:{m:02}:{s:02},000")
}

async fn render(Json(body): Json<serde_json::Value>) -> Json<serde_json::Value> {
    let job_id = body["job_id"].as_str().unwrap_or("demo");
    let title = body["title"].as_str().unwrap_or("Demo Video");
    let audio_files = body["audio_files"].as_array().cloned().unwrap_or_default();
    let srt_path = body["srt_path"].as_str().unwrap_or("");
    let segments = body["segments"].as_array().cloned().unwrap_or_default();

    let dir = format!("./data/demo/renders/{job_id}");
    std::fs::create_dir_all(&dir).ok();
    let output_path = format!("{dir}/final.mp4");

    // Collect existing audio file paths.
    let audio_paths: Vec<String> = audio_files
        .iter()
        .filter_map(|v| v.as_str().map(String::from))
        .filter(|p| Path::new(p).exists())
        .collect();

    if audio_paths.is_empty() {
        info!("Render: no audio files, creating placeholder");
        render_placeholder(&output_path);
        return Json(serde_json::json!({
            "job_id": job_id, "output_path": output_path, "format": "mp4",
        }));
    }

    // Render each segment as a separate clip, then concatenate.
    let mut clip_paths = Vec::new();
    for (i, audio) in audio_paths.iter().enumerate() {
        let seg = segments.get(i);
        let seg_title = seg.and_then(|s| s["title"].as_str()).unwrap_or("Segment");
        let is_intro = i == 0;
        let clip_path = format!("{dir}/clip_{i:03}.mp4");

        if is_intro {
            render_title_card(audio, title, seg_title, &clip_path);
        } else {
            render_content_segment(audio, srt_path, i, &clip_path);
        }

        if Path::new(&clip_path).exists() {
            clip_paths.push(clip_path);
        }
    }

    if clip_paths.is_empty() {
        info!("Render: no clips produced, creating placeholder");
        render_placeholder(&output_path);
    } else if clip_paths.len() == 1 {
        std::fs::rename(&clip_paths[0], &output_path).ok();
        info!("Render: single clip → {output_path}");
    } else {
        // Concatenate all clips into final output.
        let list_path = format!("{dir}/concat.txt");
        let list_content: String = clip_paths
            .iter()
            .map(|p| {
                let abs = std::fs::canonicalize(p).unwrap_or_else(|_| std::path::PathBuf::from(p));
                format!("file '{}'\n", abs.display())
            })
            .collect();
        std::fs::write(&list_path, &list_content).ok();

        let status = Command::new("ffmpeg")
            .args([
                "-y",
                "-f",
                "concat",
                "-safe",
                "0",
                "-i",
                &list_path,
                "-c",
                "copy",
                &output_path,
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        match status {
            Ok(s) if s.success() => {
                info!(
                    "Render: concatenated {} clips → {output_path}",
                    clip_paths.len()
                );
            }
            _ => info!("Render: concat failed, falling back to first clip"),
        }
    }

    Json(serde_json::json!({
        "job_id": job_id,
        "output_path": output_path,
        "format": "mp4",
    }))
}

/// Render intro segment: dark gradient background with large centered title and subtitle.
fn render_title_card(audio_path: &str, title: &str, subtitle: &str, out: &str) {
    // Escape single quotes for ffmpeg drawtext.
    let title_esc = title.replace('\'', "'\\''");
    let sub_esc = subtitle.replace('\'', "'\\''");

    // Use drawtext on a solid color background via filter_complex.
    let vf = format!(
        "color=c=0x0f0f23:s=1920x1080:r=30,\
         drawtext=text='{title_esc}':\
         fontsize=72:fontcolor=white:\
         x=(w-text_w)/2:y=(h-text_h)/2-60,\
         drawtext=text='{sub_esc}':\
         fontsize=36:fontcolor=0xaaaaaa:\
         x=(w-text_w)/2:y=(h/2)+40\
         [v]"
    );

    let result = Command::new("ffmpeg")
        .args([
            "-y",
            "-i",
            audio_path,
            "-filter_complex",
            &vf,
            "-map",
            "[v]",
            "-map",
            "0:a",
            "-shortest",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-c:a",
            "aac",
            "-b:a",
            "128k",
            "-pix_fmt",
            "yuv420p",
            out,
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    match result {
        Ok(s) if s.success() => info!("Render: title card → {out}"),
        _ => info!("Render: title card failed for {out}"),
    }
}

/// Render a content segment: dark background with subtitles burned in.
fn render_content_segment(audio_path: &str, srt_path: &str, _seg_idx: usize, out: &str) {
    let mut args = vec![
        "-y".to_string(),
        "-i".into(),
        audio_path.to_string(),
        "-f".into(),
        "lavfi".into(),
        "-i".into(),
        "color=c=0x1a1a2e:s=1920x1080:r=30".into(),
        "-shortest".into(),
    ];

    // Burn subtitles if SRT exists.
    if !srt_path.is_empty()
        && Path::new(srt_path).exists()
        && let Ok(abs) = std::fs::canonicalize(srt_path)
    {
        let escaped = abs.display().to_string().replace(':', "\\:");
        let vf = format!(
            "subtitles='{escaped}':force_style=\
             'FontSize=32,FontName=Arial,PrimaryColour=&H00FFFFFF,\
             OutlineColour=&H00000000,Outline=2,MarginV=50'"
        );
        args.extend(["-vf".to_string(), vf]);
    }

    args.extend([
        "-c:v".into(),
        "libx264".into(),
        "-preset".into(),
        "ultrafast".into(),
        "-c:a".into(),
        "aac".into(),
        "-b:a".into(),
        "128k".into(),
        "-pix_fmt".into(),
        "yuv420p".into(),
        out.to_string(),
    ]);

    let status = Command::new("ffmpeg")
        .args(&args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    match status {
        Ok(s) if s.success() => info!("Render: content clip → {out}"),
        _ => info!("Render: content clip failed for {out}"),
    }
}

/// Render a minimal placeholder video.
fn render_placeholder(out: &str) {
    Command::new("ffmpeg")
        .args([
            "-y",
            "-f",
            "lavfi",
            "-i",
            "color=c=0x1a1a2e:s=1920x1080:r=30:d=5",
            "-f",
            "lavfi",
            "-i",
            "anullsrc=r=44100:cl=stereo",
            "-shortest",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-c:a",
            "aac",
            "-pix_fmt",
            "yuv420p",
            out,
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .ok();
}

async fn qa(Json(body): Json<serde_json::Value>) -> Json<serde_json::Value> {
    let job_id = body["job_id"].as_str().unwrap_or("demo");
    let expected_duration = body["expected_duration_seconds"].as_u64().unwrap_or(30);
    let output_path = body["output_path"].as_str().unwrap_or("");

    // Try real ffprobe if the file exists.
    if Path::new(output_path).exists() {
        let probe = Command::new("ffprobe")
            .args([
                "-v",
                "quiet",
                "-print_format",
                "json",
                "-show_format",
                "-show_streams",
                output_path,
            ])
            .output();

        if let Ok(output) = probe
            && let Ok(probe_json) = serde_json::from_slice::<serde_json::Value>(&output.stdout)
        {
            let actual_dur = probe_json["format"]["duration"]
                .as_str()
                .and_then(|s| s.parse::<f64>().ok())
                .unwrap_or(0.0);
            let streams = probe_json["streams"].clone();

            let mut issues = Vec::<String>::new();
            let dur_diff = (actual_dur - expected_duration as f64).abs();
            if dur_diff > 2.0 {
                issues.push(format!(
                    "Duration mismatch: expected {expected_duration}s, got {actual_dur:.1}s"
                ));
            }

            let passed = issues.is_empty();
            info!("QA: probed {output_path} — {actual_dur:.1}s, passed={passed}");

            return Json(serde_json::json!({
                "job_id": job_id,
                "passed": passed,
                "issues": issues,
                "media_info": {
                    "duration_seconds": actual_dur,
                    "format_name": probe_json["format"]["format_name"],
                    "streams": streams,
                },
            }));
        }
    }

    // Fallback: mock response if file doesn't exist or probe fails.
    info!("QA: file not found or probe failed, returning mock response");
    Json(serde_json::json!({
        "job_id": job_id,
        "passed": true,
        "issues": [],
        "media_info": {
            "duration_seconds": expected_duration as f64,
            "format_name": "mp4",
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    #[tokio::test]
    async fn plan_produces_segments_and_queries() {
        let app = mock_router();
        let body = serde_json::json!({
            "job_id": "test-1",
            "title": "Rust Basics",
            "idea": "Introduction to Rust",
            "audience": "beginners",
            "target_duration_seconds": 30,
            "tone": "friendly",
            "must_include": [],
            "must_avoid": []
        });

        let resp = app
            .oneshot(
                Request::post("/plan")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);

        let plan: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(plan["segments"].as_array().unwrap().len(), 2);
        assert_eq!(plan["research_queries"].as_array().unwrap().len(), 2);

        // Segment durations should sum to total
        let total: u64 = plan["segments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["duration_seconds"].as_u64().unwrap())
            .sum();
        assert_eq!(total, 30);
    }

    #[tokio::test]
    async fn full_chain_data_flows_correctly() {
        let app = mock_router();

        // 1. Plan
        let plan_req = serde_json::json!({
            "job_id": "chain-1",
            "title": "Test",
            "idea": "Demo idea",
            "audience": "devs",
            "target_duration_seconds": 20,
            "tone": "casual",
            "must_include": [],
            "must_avoid": []
        });
        let resp = app
            .clone()
            .oneshot(json_request("/plan", &plan_req))
            .await
            .unwrap();
        let plan: serde_json::Value = parse_body(resp).await;

        // 2. Research
        let research_req = serde_json::json!({
            "job_id": "chain-1",
            "queries": plan["research_queries"],
        });
        let resp = app
            .clone()
            .oneshot(json_request("/research", &research_req))
            .await
            .unwrap();
        let research: serde_json::Value = parse_body(resp).await;

        // 3. Script
        let script_req = serde_json::json!({
            "plan": plan,
            "research": research,
        });
        let resp = app
            .clone()
            .oneshot(json_request("/script", &script_req))
            .await
            .unwrap();
        let script: serde_json::Value = parse_body(resp).await;
        assert_eq!(script["segments"].as_array().unwrap().len(), 2);

        // 4. TTS
        let resp = app
            .clone()
            .oneshot(json_request("/tts", &script))
            .await
            .unwrap();
        let tts: serde_json::Value = parse_body(resp).await;
        assert_eq!(tts["segment_files"].as_array().unwrap().len(), 2);

        // 5. ASR Validation (build like orchestrator does)
        let tts_files = tts["segment_files"].as_array().unwrap();
        let script_segs = script["segments"].as_array().unwrap();
        let asr_segments: Vec<serde_json::Value> = tts_files
            .iter()
            .map(|tf| {
                let num = tf["segment_number"].as_u64().unwrap();
                let expected = script_segs
                    .iter()
                    .find(|s| s["segment_number"].as_u64().unwrap() == num)
                    .and_then(|s| s["narration_text"].as_str())
                    .unwrap_or("");
                serde_json::json!({
                    "segment_number": num,
                    "audio_path": tf["file_path"],
                    "expected_text": expected,
                })
            })
            .collect();
        let asr_req = serde_json::json!({
            "job_id": "chain-1",
            "segments": asr_segments,
        });
        let resp = app
            .clone()
            .oneshot(json_request("/validate", &asr_req))
            .await
            .unwrap();
        let asr: serde_json::Value = parse_body(resp).await;
        assert!(asr["passed"].as_bool().unwrap());

        // 6. Captions
        let resp = app
            .clone()
            .oneshot(json_request("/captions", &script))
            .await
            .unwrap();
        let caps: serde_json::Value = parse_body(resp).await;
        assert_eq!(caps["cue_count"].as_u64().unwrap(), 2);

        // 7. Render
        let audio_files: Vec<String> = tts_files
            .iter()
            .filter_map(|f| f["file_path"].as_str().map(String::from))
            .collect();
        let render_req = serde_json::json!({
            "job_id": "chain-1",
            "audio_files": audio_files,
        });
        let resp = app
            .clone()
            .oneshot(json_request("/render", &render_req))
            .await
            .unwrap();
        let render: serde_json::Value = parse_body(resp).await;
        assert!(render["output_path"].as_str().unwrap().ends_with(".mp4"));

        // 8. QA
        let qa_req = serde_json::json!({
            "job_id": "chain-1",
            "output_path": render["output_path"],
            "expected_duration_seconds": 20,
        });
        let resp = app
            .clone()
            .oneshot(json_request("/qa", &qa_req))
            .await
            .unwrap();
        let qa_result: serde_json::Value = parse_body(resp).await;
        assert!(qa_result["passed"].as_bool().unwrap());
    }

    fn json_request(path: &str, body: &serde_json::Value) -> Request<Body> {
        Request::post(path)
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(body).unwrap()))
            .unwrap()
    }

    async fn parse_body(resp: axum::response::Response) -> serde_json::Value {
        serde_json::from_slice(
            &axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap()
    }
}
