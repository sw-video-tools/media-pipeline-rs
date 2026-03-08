# Architecture

## Layers

### 1. Context intelligence layer
Handled by `rlm-client` against your external `rlm-project` service.

Use it for:

- compact working-set creation from large corpora
- evidence packet generation
- long-transcript / long-history exploration
- failure investigation over logs and validation artifacts

### 2. Execution backbone
Handled by Rust services plus `provider-router`.

Use it for:

- model/provider routing
- multimodal generation calls
- embeddings / retrieval
- task orchestration
- queue-driven stage execution

### 3. Deterministic truth layer
Handled by `media-tools` and validators.

Use it for:

- ffprobe metadata
- ffmpeg rendering
- caption parsing
- transcript diffing
- loudness, timing, and other measurable properties

## Core rule

The system should be **artifact-centric**, not prompt-centric.

Every stage reads input artifact references and writes output artifact references.
The model should not be the only memory of what happened.
