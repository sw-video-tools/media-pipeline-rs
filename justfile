set dotenv-load

run-api:
    cargo run -p api-gateway

run-orchestrator:
    cargo run -p orchestrator-service

fmt:
    cargo fmt --all

check:
    cargo check --workspace
