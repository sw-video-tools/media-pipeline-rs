# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Development Commands

```bash
# Build & check
just check                # cargo check --workspace
just fmt                  # cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings

# Run services
just run-api              # cargo run -p api-gateway
just run-orchestrator     # cargo run -p orchestrator-service

# Run a specific service
cargo run -p <service-name>

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

1. **Context Intelligence** (`rlm-client`): Talks to external RLM service for research context, evidence packets, transcript exploration
2. **Execution Backbone** (15 Axum services + `provider-router`): Model/provider routing, multimodal generation, queue-driven task orchestration
3. **Deterministic Truth** (`media-tools`, `validator-core`): ffprobe/ffmpeg wrappers, loudness/timing/caption validation — generators and validators are always separate services

### Core Design Principle: Artifact-Centric

Every pipeline stage reads input artifact refs and writes output artifact refs. All artifacts are wrapped in `ArtifactEnvelope<T>` (defined in `pipeline-types`) which tracks: producer info, input/evidence refs, validation state, and content hash. The model is never the sole memory of what happened.

### Workspace Layout

- **`crates/`** — Shared libraries: `pipeline-types` (core types, `Stage`, `JobStatus`, `ArtifactEnvelope`), `artifact-store`, `job-queue`, `provider-router`, `rlm-client`, `media-tools`, `validator-core`, `prompt-templates`, `telemetry`
- **`services/`** — 15 Axum microservices (api-gateway is the entry point at `:3000` with `/healthz` and `/jobs` routes)
- **`apps/`** — `operator-cli` (clap), `review-web` (web UI)
- **`config/`** — `local.toml`, `production.toml`, `models.toml` (model-to-provider mappings)
- **`jobs/`** — Sample job JSON manifests
- **`docs/`** — Architecture, validation strategy, provider routing, deployment docs

### Pipeline Stages (in order)

Planning → Research → Script → Storyboard → VisualGeneration → MusicGeneration → TTS → ASR Validation → Captions → RenderPreview → QA Preview → RenderFinal → QA Final → Complete

### Service Pattern

Every service follows: async tokio main → `telemetry::init()` → Axum router with `/healthz` + domain routes. Services depend on `pipeline-types` and `telemetry` plus domain-specific crates.

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
- **`.env.example`**: Environment variables including `RUST_LOG`, `ARTIFACT_ROOT`, `SQLITE_DB`, `FFMPEG_BIN`
