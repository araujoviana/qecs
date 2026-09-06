#!/usr/bin/env bash
set -euo pipefail
git config core.hooksPath .githooks
chmod +x .githooks/*
echo "qecs git hooks installed (core.hooksPath=.githooks)"
