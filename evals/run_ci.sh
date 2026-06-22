#!/usr/bin/env bash
# Local mirror of CI + harness replay tier.
set -euo pipefail
cd "$(dirname "$0")/.."
echo "==> build"
cargo build --no-default-features
echo "==> unit + replay tests"
cargo test --no-default-features
echo "==> clippy"
cargo clippy --no-default-features -- -D warnings
echo "==> PASS: ci + replay"