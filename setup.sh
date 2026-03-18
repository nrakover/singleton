#!/usr/bin/env bash
set -euo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

echo "Setting up singleton..."

# Install dependencies
uv sync

# Create state directories
mkdir -p ~/.singleton/workers/default
mkdir -p ~/.singleton/threads

# Make hook scripts executable
chmod +x "$REPO_DIR/hooks/"*.sh

echo "Setup complete."
echo "Run 'singleton' to start."
