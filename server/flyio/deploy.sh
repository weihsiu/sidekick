#!/usr/bin/env bash
set -euo pipefail

# Deploy sidekick-server to Fly.io using local Docker build.
# Run from the project root (parent of client/ and server/).

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
FLY_TOML="$SCRIPT_DIR/fly.toml"
DOCKERFILE="$SCRIPT_DIR/Dockerfile"
APP_NAME="sidekick-server"
IMAGE="registry.fly.io/$APP_NAME:latest"

cd "$PROJECT_ROOT"

echo "==> Building Docker image (linux/amd64)..."
docker build \
  --platform=linux/amd64 \
  -f "$DOCKERFILE" \
  -t "$IMAGE" \
  .

echo "==> Authenticating with Fly.io registry..."
fly auth docker

echo "==> Pushing image..."
docker push "$IMAGE"

echo "==> Deploying on Fly.io..."
fly deploy --config "$FLY_TOML" --image "$IMAGE"

echo "==> Done!"
