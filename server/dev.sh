#!/usr/bin/env bash
# Start the server for local development.
# Usage: ./dev.sh

set -euo pipefail
cd "$(dirname "$0")"

export BASE_URL=http://localhost:3000
export FRONTEND_URL=http://localhost:5173

cargo run "$@"
