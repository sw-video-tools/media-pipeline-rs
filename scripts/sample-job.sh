#!/usr/bin/env bash
set -euo pipefail
curl -X POST http://127.0.0.1:3000/jobs \
  -H 'content-type: application/json' \
  --data @jobs/sample-short-video.json
