# Deployment Notes

Suggested local-first deployment:

- SQLite for metadata
- local filesystem for artifact binaries
- one Axum process per service during development
- later, containerize workers and add Postgres + object storage

Suggested first production move:

- keep API gateway, orchestrator, and render service isolated
- introduce a real queue and object store
- add OpenTelemetry exports and request IDs
