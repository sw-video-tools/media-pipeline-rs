//! Mock implementations of all 8 pipeline stage services.
//!
//! Each handler produces realistic output that chains correctly to
//! downstream stages. No external LLMs, TTS engines, or media tools
//! are required — this is a pure Rust simulation of the pipeline.

use axum::routing::{get, post};
use axum::{Json, Router};

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

    // Create dummy audio file paths (no real audio generated).
    let segment_files: Vec<serde_json::Value> = segments
        .iter()
        .map(|seg| {
            let num = seg["segment_number"].as_u64().unwrap_or(1);
            serde_json::json!({
                "segment_number": num,
                "file_path": format!("./data/demo/tts/{job_id}_seg{num:03}.mp3")
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

    Json(serde_json::json!({
        "job_id": job_id,
        "srt_path": format!("./data/demo/captions/{job_id}.srt"),
        "cue_count": segments.len(),
    }))
}

async fn render(Json(body): Json<serde_json::Value>) -> Json<serde_json::Value> {
    let job_id = body["job_id"].as_str().unwrap_or("demo");

    Json(serde_json::json!({
        "job_id": job_id,
        "output_path": format!("./data/demo/renders/{job_id}.mp4"),
        "format": "mp4",
    }))
}

async fn qa(Json(body): Json<serde_json::Value>) -> Json<serde_json::Value> {
    let job_id = body["job_id"].as_str().unwrap_or("demo");
    let expected_duration = body["expected_duration_seconds"].as_u64().unwrap_or(30);

    Json(serde_json::json!({
        "job_id": job_id,
        "passed": true,
        "issues": [],
        "media_info": {
            "duration_seconds": expected_duration as f64,
            "format_name": "mp4",
            "streams": [
                {
                    "index": 0,
                    "codec_type": "video",
                    "codec_name": "h264",
                    "width": 1920,
                    "height": 1080
                },
                {
                    "index": 1,
                    "codec_type": "audio",
                    "codec_name": "aac",
                    "sample_rate": "44100",
                    "channels": 2
                }
            ]
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
