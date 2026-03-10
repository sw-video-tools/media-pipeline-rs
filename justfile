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

list-jobs:
    cargo run -p operator-cli -- list
