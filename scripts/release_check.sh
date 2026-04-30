#!/usr/bin/env bash
set -euo pipefail

# Reproducible pre-release checks:
# - lockfile-resolved build and tests
# - formatting and lint gates
# - deterministic release artifact build

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${ROOT_DIR}"

if ! command -v cargo >/dev/null 2>&1; then
  echo "error: cargo not found in PATH"
  exit 1
fi

echo "[1/4] cargo fmt --all -- --check"
cargo fmt --all -- --check

if [[ "${NIGHTINDEX_STRICT_CLIPPY:-0}" == "1" ]]; then
  echo "[2/4] cargo clippy --locked --all-targets -- -D warnings"
  cargo clippy --locked --all-targets -- -D warnings
else
  echo "[2/4] cargo clippy --locked --all-targets (non-blocking; set NIGHTINDEX_STRICT_CLIPPY=1 to enforce -D warnings)"
  cargo clippy --locked --all-targets || true
fi

echo "[3/4] cargo test --locked --all-targets"
cargo test --locked --all-targets

echo "[4/4] cargo build --release --locked"
cargo build --release --locked

BIN_PATH="${ROOT_DIR}/target/release/nightindex"
if [[ ! -x "${BIN_PATH}" ]]; then
  echo "error: expected release binary at ${BIN_PATH}"
  exit 1
fi

echo "ok: pre-release checks passed"
