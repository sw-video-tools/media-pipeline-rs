set dotenv-load

# Build & check
check:
    cargo check --workspace

fmt:
    cargo fmt --all

clippy:
    cargo clippy --all-targets --all-features -- -D warnings

test *ARGS:
    cargo test {{ARGS}}

# Checkpoint: test + clippy + fmt
checkpoint:
    cargo test
    cargo clippy --all-targets --all-features -- -D warnings
    cargo fmt --all

# Start api-gateway + orchestrator (the two always-on processes)
dev:
    #!/usr/bin/env bash
    set -euo pipefail
    trap 'kill $(jobs -p) 2>/dev/null' EXIT
    cargo run -p api-gateway &
    cargo run -p orchestrator-service &
    wait

# Run individual services
run-api:
    cargo run -p api-gateway

run-orchestrator:
    cargo run -p orchestrator-service

run-planner:
    cargo run -p planner-service

run-research:
    cargo run -p research-service

run-script:
    cargo run -p script-service

run-tts:
    cargo run -p tts-service

run-asr:
    cargo run -p asr-validation-service

run-captions:
    cargo run -p captions-service

run-render:
    cargo run -p render-service

run-qa:
    cargo run -p qa-service

# Operator CLI
submit FILE:
    cargo run -p operator-cli -- submit {{FILE}}

status JOB_ID:
    cargo run -p operator-cli -- status {{JOB_ID}}

detail JOB_ID:
    cargo run -p operator-cli -- detail {{JOB_ID}}

logs JOB_ID:
    cargo run -p operator-cli -- logs {{JOB_ID}}

retry JOB_ID:
    cargo run -p operator-cli -- retry {{JOB_ID}}

delete-job JOB_ID:
    cargo run -p operator-cli -- delete {{JOB_ID}}

list-jobs:
    cargo run -p operator-cli -- list

health:
    cargo run -p operator-cli -- health

# Demo: run mock services + api-gateway + orchestrator for smoke testing
demo-services:
    #!/usr/bin/env bash
    set -euo pipefail
    trap 'kill $(jobs -p) 2>/dev/null' EXIT
    MOCK_PORT=${MOCK_PORT:-3200}
    mkdir -p data
    echo "Starting mock services on port $MOCK_PORT..."
    cargo run -p demo-runner -- mock &
    sleep 1
    echo "Starting api-gateway + orchestrator (pointed at mock services)..."
    PLANNER_URL=http://127.0.0.1:$MOCK_PORT \
    RESEARCH_URL=http://127.0.0.1:$MOCK_PORT \
    SCRIPT_URL=http://127.0.0.1:$MOCK_PORT \
    TTS_URL=http://127.0.0.1:$MOCK_PORT \
    ASR_URL=http://127.0.0.1:$MOCK_PORT \
    CAPTIONS_URL=http://127.0.0.1:$MOCK_PORT \
    RENDER_URL=http://127.0.0.1:$MOCK_PORT \
    QA_URL=http://127.0.0.1:$MOCK_PORT \
    cargo run -p api-gateway &
    PLANNER_URL=http://127.0.0.1:$MOCK_PORT \
    RESEARCH_URL=http://127.0.0.1:$MOCK_PORT \
    SCRIPT_URL=http://127.0.0.1:$MOCK_PORT \
    TTS_URL=http://127.0.0.1:$MOCK_PORT \
    ASR_URL=http://127.0.0.1:$MOCK_PORT \
    CAPTIONS_URL=http://127.0.0.1:$MOCK_PORT \
    RENDER_URL=http://127.0.0.1:$MOCK_PORT \
    QA_URL=http://127.0.0.1:$MOCK_PORT \
    cargo run -p orchestrator-service &
    wait

# Run the demo end-to-end (requires demo-services running in another terminal)
demo-run:
    cargo run -p demo-runner

# Quick demo: start everything and run the smoke test
demo:
    #!/usr/bin/env bash
    set -euo pipefail
    MOCK_PORT=${MOCK_PORT:-3200}
    trap 'kill $(jobs -p) 2>/dev/null' EXIT
    echo "=== Starting demo infrastructure ==="
    mkdir -p data
    cargo run -p demo-runner -- mock &
    sleep 1
    PLANNER_URL=http://127.0.0.1:$MOCK_PORT \
    RESEARCH_URL=http://127.0.0.1:$MOCK_PORT \
    SCRIPT_URL=http://127.0.0.1:$MOCK_PORT \
    TTS_URL=http://127.0.0.1:$MOCK_PORT \
    ASR_URL=http://127.0.0.1:$MOCK_PORT \
    CAPTIONS_URL=http://127.0.0.1:$MOCK_PORT \
    RENDER_URL=http://127.0.0.1:$MOCK_PORT \
    QA_URL=http://127.0.0.1:$MOCK_PORT \
    cargo run -p api-gateway &
    PLANNER_URL=http://127.0.0.1:$MOCK_PORT \
    RESEARCH_URL=http://127.0.0.1:$MOCK_PORT \
    SCRIPT_URL=http://127.0.0.1:$MOCK_PORT \
    TTS_URL=http://127.0.0.1:$MOCK_PORT \
    ASR_URL=http://127.0.0.1:$MOCK_PORT \
    CAPTIONS_URL=http://127.0.0.1:$MOCK_PORT \
    RENDER_URL=http://127.0.0.1:$MOCK_PORT \
    QA_URL=http://127.0.0.1:$MOCK_PORT \
    cargo run -p orchestrator-service &
    sleep 3
    echo ""
    cargo run -p demo-runner
