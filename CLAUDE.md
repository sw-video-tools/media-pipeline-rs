# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Development Commands

```bash
# Build & check
just check                # cargo check --workspace
just fmt                  # cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings

# Run services
just run-api              # cargo run -p api-gateway (port 3190)
just run-orchestrator     # cargo run -p orchestrator-service (port 3199)

# Run a specific service
cargo run -p <service-name>

# Operator CLI
cargo run -p operator-cli -- submit jobs/sample-explainer.json
cargo run -p operator-cli -- status <job-id>
cargo run -p operator-cli -- list

# Tests
cargo test                # all tests
cargo test -p <crate>     # single crate
cargo test <test_name>    # single test
cargo test -- --nocapture # with stdout

# Bootstrap (fmt + check)
./scripts/bootstrap.sh
```

## Checkpoint Process

When a **checkpoint** is requested, run this sequence in order — all must pass:

1. `cargo test`
2. `cargo clippy --all-targets --all-features -- -D warnings` (zero warnings, never suppress with `#[allow(...)]`)
3. `cargo fmt --all`
4. Update docs if needed
5. Commit and push

## Architecture

This is a **Rust workspace** (edition 2024) implementing an AI-powered video production pipeline with 28 crates organized in three layers:

### Three-Layer Design

1. **Context Intelligence** (`rlm-client`): Talks to external RLM service for research context, evidence packets, transcript exploration. Falls back to stub responses when RLM is unreachable.
2. **Execution Backbone** (15 Axum services + `provider-router`): Model/provider routing, multimodal generation, queue-driven task orchestration
3. **Deterministic Truth** (`media-tools`, `validator-core`): ffprobe/ffmpeg wrappers, loudness/timing/caption validation — generators and validators are always separate services

### Core Design Principle: Artifact-Centric

Every pipeline stage reads input artifact refs and writes output artifact refs. All artifacts are wrapped in `ArtifactEnvelope<T>` (defined in `pipeline-types`) which tracks: producer info, input/evidence refs, validation state, and content hash. The model is never the sole memory of what happened.

### Workspace Layout

- **`crates/`** — Shared libraries:
  - `pipeline-types`: Core types (`Stage`, `JobStatus`, `ArtifactEnvelope`, `ProjectPlan`, `NarrationScript`, `ResearchPacket`). `Stage` has `next_mvp()` for pipeline sequencing and `Display` impl.
  - `artifact-store`: SQLite-backed job persistence (save/get/list/update jobs)
  - `job-queue`: SQLite-backed FIFO queue with claim semantics (enqueue/dequeue/acknowledge)
  - `provider-router`: TOML-config-driven model routing (`from_file()`, `from_toml()`, `route_for()`)
  - `rlm-client`: HTTP client for RLM service with stub fallback
  - `media-tools`: `ffprobe` module (probe/duration/has_audio/has_video) + `ffmpeg` module (concat_audio/render_slideshow/measure_loudness)
  - `validator-core`: `require()` helper producing `ValidationIssue` lists
  - `prompt-templates`, `telemetry`
- **`services/`** — 15 Axum microservices (see Service Ports below)
- **`apps/`** — `operator-cli` (submit/status/list via API gateway), `review-web` (stub)
- **`config/`** — `local.toml`, `production.toml`, `models.toml`
- **`jobs/`** — Sample job JSON manifests
- **`docs/`** — Architecture, validation strategy, provider routing, deployment docs

### MVP Pipeline (implemented)

Planning → Research → Script → TTS → ASR Validation → Captions → RenderFinal → QA Final → Complete

Driven by the **orchestrator-service** which polls the job-queue and calls each service in order. Non-MVP stages (Storyboard, VisualGeneration, MusicGeneration, Preview) are defined but skip to the next MVP stage.

### Service Ports

| Port | Service | Endpoint |
|------|---------|----------|
| 3190 | api-gateway | GET/POST /jobs, GET /jobs/:id |
| 3191 | planner-service | POST /plan |
| 3192 | research-service | POST /research |
| 3193 | script-service | POST /script |
| 3194 | tts-service | POST /tts |
| 3195 | asr-validation-service | POST /validate |
| 3196 | captions-service | POST /captions |
| 3197 | render-service | POST /render |
| 3198 | qa-service | POST /qa |
| 3199 | orchestrator-service | GET /status |

### Service Pattern

Every service follows: async tokio main → `telemetry::init()` → Axum router with `/healthz` + domain routes. Services depend on `pipeline-types` and `telemetry` plus domain-specific crates. LLM-calling services (planner, script) use `provider-router` + `reqwest` to call OpenAI chat completions and parse JSON from responses (with markdown fence extraction and fallback).

### Data Flow

1. **api-gateway** receives `POST /jobs`, saves to `artifact-store`, enqueues in `job-queue`
2. **orchestrator-service** dequeues entries, calls the stage's service, updates job status, enqueues next stage
3. Each service processes its input and returns a JSON result
4. On failure, orchestrator marks job as `Failed` and clears the queue entry

## Code Quality Standards

- Rust 2024 edition idioms; use inline format args: `format!("{name}")` not `format!("{}", name)`
- Module docs use `//!`, item docs use `///`
- Files under 500 lines (prefer 200-300), functions under 50 lines (prefer 10-30)
- Max 3 TODOs per file; never commit FIXMEs
- TDD workflow: Red (failing test) → Green (minimal code) → Refactor
- Workspace dependencies defined in root `Cargo.toml` — use `dep.workspace = true` in member crates

## Configuration

- **`config/local.toml`**: API bind address, artifact root path, RLM base URL, default providers
- **`config/models.toml`**: Maps capabilities (planning, script, tts, asr) to provider+model pairs
- **`.env.example`**: Environment variables including `RUST_LOG`, `SQLITE_DB`, `QUEUE_DB`, `FFMPEG_BIN`, `OPENAI_API_KEY`
- Service URLs configurable via env vars: `PLANNER_URL`, `RESEARCH_URL`, `SCRIPT_URL`, `TTS_URL`, `ASR_URL`, `CAPTIONS_URL`, `RENDER_URL`, `QA_URL`
