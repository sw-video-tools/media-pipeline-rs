# media-pipeline-rs

A Rust-first, artifact-centric AI media production pipeline.

This workspace is a starter skeleton for a system that combines:

- **RLM** (Recursive Language Model style context inspection) as the long-context reasoning layer
- **Rig** as the provider / multimodal execution backbone
- **Deterministic media tooling** (`ffmpeg`, `ffprobe`, subtitle tooling, validators) as the source of truth
- **Typed Rust artifacts** as memory, provenance, and caching

## Goals

- Keep context **outside** prompts whenever possible
- Reduce context rot via compact working sets and evidence packets
- Avoid hallucinations by preferring deterministic tools before model guesses
- Support idea -> script -> storyboard -> visuals -> music -> TTS -> validation -> captions -> render -> QA
- Make re-runs and partial rebuilds cheap and explicit

## Current state

This zip contains:

- Cargo workspace layout
- Shared Rust types and crate stubs
- Axum service stubs
- Sample job manifests
- Basic docs describing architecture, schemas, routing, and validation strategy

It is intentionally **scaffold-first**, not a fully working production system.

## Suggested first milestone

Implement only these stages first:

1. planner-service
2. research-service
3. script-service
4. tts-service
5. asr-validation-service
6. captions-service
7. render-service
8. qa-service

That gives you a viable MVP: **idea -> narration -> validation -> captions -> simple slideshow render**.

## Suggested next tasks

- Add SQLite-backed artifact metadata
- Add local file/object-store persistence
- Wire `provider-router` to Rig clients
- Wire `rlm-client` to your `rlm-project` endpoints
- Implement `media-tools` wrappers around `ffprobe` and `ffmpeg`
- Add stage-by-stage validators and retry rules

