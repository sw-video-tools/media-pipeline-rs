//! Demo runner: end-to-end pipeline smoke test.
//!
//! Starts mock services for all 8 pipeline stages on a single port,
//! then submits a demo job through the real api-gateway + orchestrator
//! and monitors it to completion.
//!
//! Usage:
//!   # Terminal 1: start api-gateway + orchestrator pointed at mock services
//!   just demo-services
//!
//!   # Terminal 2: run the demo
//!   cargo run -p demo-runner
//!
//! Or use `just demo` to run everything in one shot.

mod mock_services;

use std::time::Duration;

use anyhow::{Context, Result};
use tracing::{error, info, warn};

/// Demo job template: a 30-second micro-explainer.
fn demo_job() -> serde_json::Value {
    serde_json::json!({
        "title": "Rust Memory Safety in 30 Seconds",
        "idea": "Explain how Rust prevents memory bugs using ownership and borrowing, \
                 with a simple example comparing C and Rust",
        "audience": "developers new to Rust",
        "target_duration_seconds": 30,
        "tone": "friendly and approachable",
        "must_include": ["ownership", "borrowing", "no garbage collector"],
        "must_avoid": ["unsafe code details"]
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    telemetry::init();

    let mock_port: u16 = std::env::var("MOCK_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3200);
    let api_url = std::env::var("API_URL").unwrap_or_else(|_| "http://127.0.0.1:3190".into());

    let mode = std::env::args().nth(1).unwrap_or_default();

    match mode.as_str() {
        "mock" => run_mock_server(mock_port).await,
        _ => run_demo(&api_url, mock_port).await,
    }
}

/// Start the mock services server (blocks forever).
async fn run_mock_server(port: u16) -> Result<()> {
    let app = mock_services::mock_router();
    let addr = format!("0.0.0.0:{port}");
    info!("mock services listening on {addr}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

/// Run the demo: submit a job and poll until done.
async fn run_demo(api_url: &str, mock_port: u16) -> Result<()> {
    let client = reqwest::Client::new();

    // Phase 1: Check if mock services are running.
    println!("\n=== Demo Runner: Pipeline Smoke Test ===\n");
    print_step(1, "Checking mock services...");

    let mock_url = format!("http://127.0.0.1:{mock_port}");
    match client
        .get(format!("{mock_url}/healthz"))
        .timeout(Duration::from_secs(2))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => {
            println!("   Mock services: UP at {mock_url}");
        }
        _ => {
            println!("   Mock services not running at {mock_url}");
            println!("   Starting embedded mock server...\n");

            // Spawn mock server in background
            let port = mock_port;
            tokio::spawn(async move {
                let app = mock_services::mock_router();
                let addr = format!("0.0.0.0:{port}");
                let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
                axum::serve(listener, app).await.unwrap();
            });

            // Wait for it to be ready
            for _ in 0..20 {
                tokio::time::sleep(Duration::from_millis(100)).await;
                if client
                    .get(format!("{mock_url}/healthz"))
                    .timeout(Duration::from_secs(1))
                    .send()
                    .await
                    .is_ok()
                {
                    break;
                }
            }
            println!("   Mock services: STARTED at {mock_url}");
        }
    }

    // Phase 2: Check api-gateway
    print_step(2, "Checking api-gateway...");
    match client
        .get(format!("{api_url}/healthz"))
        .timeout(Duration::from_secs(2))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => {
            println!("   API gateway: UP at {api_url}");
        }
        _ => {
            error!("API gateway not reachable at {api_url}");
            println!("   API gateway: DOWN at {api_url}");
            println!();
            println!("   Start the api-gateway and orchestrator first:");
            println!("     just demo-services");
            println!();
            println!("   Or set MOCK_PORT and all service URLs to point to mock:");
            println!("     PLANNER_URL=http://127.0.0.1:{mock_port} \\");
            println!("     RESEARCH_URL=http://127.0.0.1:{mock_port} \\");
            println!("     ... etc");
            anyhow::bail!("api-gateway not reachable");
        }
    }

    // Phase 3: Submit demo job
    print_step(3, "Submitting demo job...");
    let job_body = demo_job();
    println!("   Title: {}", job_body["title"].as_str().unwrap());
    println!(
        "   Duration: {}s",
        job_body["target_duration_seconds"].as_u64().unwrap()
    );

    let resp = client
        .post(format!("{api_url}/jobs"))
        .json(&job_body)
        .send()
        .await
        .context("failed to submit job")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("job submission failed: {status}: {text}");
    }

    let created: serde_json::Value = resp.json().await?;
    let job_id = created["job_id"]
        .as_str()
        .context("no job_id in response")?;
    println!("   Job ID: {job_id}");
    println!("   Status: {}", created["status"]);

    // Phase 4: Poll for completion
    print_step(4, "Monitoring pipeline progress...");
    let stages = [
        "Planning",
        "Research",
        "Script",
        "Tts",
        "AsrValidation",
        "Captions",
        "RenderFinal",
        "QaFinal",
    ];
    let mut last_stage = String::new();
    let mut last_status = String::new();

    let timeout = Duration::from_secs(120);
    let start = std::time::Instant::now();

    loop {
        if start.elapsed() > timeout {
            error!("demo timed out after {timeout:?}");
            anyhow::bail!("demo timed out waiting for job completion");
        }

        tokio::time::sleep(Duration::from_millis(500)).await;

        let resp = client.get(format!("{api_url}/jobs/{job_id}")).send().await;

        let resp = match resp {
            Ok(r) if r.status().is_success() => r,
            _ => continue,
        };

        let job: serde_json::Value = match resp.json().await {
            Ok(j) => j,
            Err(_) => continue,
        };

        let status = job["status"].as_str().unwrap_or("?").to_string();
        let stage = job["current_stage"].as_str().unwrap_or("?").to_string();

        if stage != last_stage || status != last_status {
            let stage_num = stages.iter().position(|s| *s == stage).map(|i| i + 1);
            let progress = stage_num
                .map(|n| format!("[{n}/8]"))
                .unwrap_or_else(|| "[---]".into());
            println!("   {progress} {status:<12} {stage}");
            last_stage = stage.clone();
            last_status = status.clone();
        }

        match status.as_str() {
            "Completed" => break,
            "Failed" => {
                println!();
                warn!("Job failed at stage {stage}");

                // Fetch events for diagnosis
                if let Ok(resp) = client
                    .get(format!("{api_url}/jobs/{job_id}/events"))
                    .send()
                    .await
                    && let Ok(events) = resp.json::<Vec<serde_json::Value>>().await
                {
                    println!("   Event log:");
                    for event in events.iter().rev().take(5).rev() {
                        let level = event["level"].as_str().unwrap_or("?");
                        let msg = event["message"].as_str().unwrap_or("?");
                        println!("   [{level}] {msg}");
                    }
                }
                anyhow::bail!("job failed at stage {stage}");
            }
            _ => {}
        }
    }

    // Phase 5: Show results
    print_step(5, "Fetching results...");

    let resp = client
        .get(format!("{api_url}/jobs/{job_id}/detail"))
        .send()
        .await
        .context("failed to fetch detail")?;
    let detail: serde_json::Value = resp.json().await?;

    let completed = detail["completed_stages"]
        .as_array()
        .map(|a| a.len())
        .unwrap_or(0);
    println!("   Completed stages: {completed}/8");

    if let Some(outputs) = detail["stage_outputs"].as_object() {
        println!();
        println!("   Stage outputs:");
        for (stage, output) in outputs {
            let summary = summarize_output(stage, output);
            println!("   {stage:<16} {summary}");
        }
    }

    // Phase 6: Show events timeline
    print_step(6, "Event timeline...");
    if let Ok(resp) = client
        .get(format!("{api_url}/jobs/{job_id}/events"))
        .send()
        .await
        && let Ok(events) = resp.json::<Vec<serde_json::Value>>().await
    {
        for event in &events {
            let ts = event["timestamp"].as_str().unwrap_or("?");
            let level = event["level"].as_str().unwrap_or("?");
            let msg = event["message"].as_str().unwrap_or("?");
            let tag = match level {
                "error" => "ERR ",
                "warn" => "WARN",
                _ => "INFO",
            };
            // Show just the time portion
            let time = ts.split('T').nth(1).unwrap_or(ts);
            let time = time.trim_end_matches('Z');
            println!("   {time}  [{tag}]  {msg}");
        }
        println!("   {} event(s) total", events.len());
    }

    let elapsed = start.elapsed();
    println!();
    println!("=== Demo complete in {:.1}s ===", elapsed.as_secs_f64());
    println!();
    println!("What this exercised:");
    println!("  - API gateway: job creation, status polling, detail retrieval");
    println!("  - Orchestrator: sequential dispatch through all 8 stages");
    println!("  - Data flow: plan → research → script → TTS → ASR → captions → render → QA");
    println!("  - Artifact store: job persistence, stage outputs, event log");
    println!("  - Job queue: enqueue/dequeue/acknowledge lifecycle");
    println!("  - TTS: real WAV audio with per-segment tones and chime transitions");
    println!("  - Captions: real SRT file with timed subtitle cues");
    println!("  - Render: real MP4 with title card, burned subtitles, crossfade transitions");
    println!("  - QA: real ffprobe analysis of rendered video (duration, codecs, streams)");
    println!();
    println!("Gaps remaining (needs real implementation):");
    println!("  - No LLM calls: script/plan text is templated, not generated");
    println!("  - No real TTS voice: audio is sine-wave tones, not speech");
    println!("  - No real media assets (images, music, b-roll) in the pipeline");
    println!("  - ASR validation is mocked (returns expected text as transcript)");

    Ok(())
}

fn print_step(n: u32, msg: &str) {
    println!("Step {n}: {msg}");
}

fn summarize_output(stage: &str, output: &serde_json::Value) -> String {
    match stage {
        "Planning" => {
            let segs = output["segments"].as_array().map(|a| a.len()).unwrap_or(0);
            let queries = output["research_queries"]
                .as_array()
                .map(|a| a.len())
                .unwrap_or(0);
            format!("{segs} segments, {queries} research queries")
        }
        "Research" => {
            let packets = output.as_array().map(|a| a.len()).unwrap_or(1);
            format!("{packets} research packet(s)")
        }
        "Script" => {
            let segs = output["segments"].as_array().map(|a| a.len()).unwrap_or(0);
            format!("{segs} narration segments")
        }
        "Tts" => {
            let files = output["segment_files"]
                .as_array()
                .map(|a| a.len())
                .unwrap_or(0);
            format!("{files} audio file(s)")
        }
        "AsrValidation" => {
            let passed = output["passed"].as_bool().unwrap_or(false);
            if passed {
                "PASSED".into()
            } else {
                "FAILED".into()
            }
        }
        "Captions" => {
            let cues = output["cue_count"].as_u64().unwrap_or(0);
            format!("{cues} caption cue(s)")
        }
        "RenderFinal" => {
            let path = output["output_path"].as_str().unwrap_or("?");
            let fmt = output["format"].as_str().unwrap_or("?");
            format!("{fmt} → {path}")
        }
        "QaFinal" => {
            let passed = output["passed"].as_bool().unwrap_or(false);
            let issues = output["issues"].as_array().map(|a| a.len()).unwrap_or(0);
            if passed {
                "PASSED (0 issues)".into()
            } else {
                format!("FAILED ({issues} issue(s))")
            }
        }
        _ => {
            let s = serde_json::to_string(output).unwrap_or_default();
            if s.len() > 80 {
                format!("{}...", &s[..77])
            } else {
                s
            }
        }
    }
}
