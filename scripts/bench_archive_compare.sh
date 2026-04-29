#!/usr/bin/env bash
set -euo pipefail

# Lightweight benchmark helper for archive-recursive-compare.
# Usage:
#   scripts/bench_archive_compare.sh /path/left.sqlite /path/right.sqlite left_label right_label [runs]

if [[ "${1:-}" == "" || "${2:-}" == "" || "${3:-}" == "" || "${4:-}" == "" ]]; then
  echo "usage: $0 <left_db> <right_db> <left_label> <right_label> [runs]"
  exit 1
fi

LEFT_DB="$1"
RIGHT_DB="$2"
LEFT_LABEL="$3"
RIGHT_LABEL="$4"
RUNS="${5:-3}"

if ! command -v cargo >/dev/null 2>&1; then
  echo "cargo not found"
  exit 1
fi

if command -v hyperfine >/dev/null 2>&1; then
  hyperfine --warmup 1 --runs "${RUNS}" \
    "cargo run --quiet -- archive-recursive-compare --left-db '${LEFT_DB}' --right-db '${RIGHT_DB}' --left '${LEFT_LABEL}' --right '${RIGHT_LABEL}' --max-bucket-items 2000 >/dev/null"
else
  echo "hyperfine not found; running ${RUNS} timed iterations with /usr/bin/time"
  for i in $(seq 1 "${RUNS}"); do
    echo "run ${i}/${RUNS}"
    /usr/bin/time -f "elapsed=%E user=%U sys=%S maxrss_kb=%M" \
      cargo run --quiet -- archive-recursive-compare \
        --left-db "${LEFT_DB}" \
        --right-db "${RIGHT_DB}" \
        --left "${LEFT_LABEL}" \
        --right "${RIGHT_LABEL}" \
        --max-bucket-items 2000 >/dev/null
  done
fi
