# Nightindex

Nightindex is a recovery-first file tree indexer, comparer, and copy engine for messy data rescue jobs.
It is built for damaged disks, incomplete backups, renamed project trees, mixed archive dumps, and large
collections where `rsync` or `rclone` alone cannot tell what is already recovered.

Use `nightindex` when paths are unreliable but content, fingerprints, archive shape, and folder context can
still identify the best copy to keep.

## What It Does

- Scans huge directory trees into SQLite manifests with optional hashing and persistent fingerprint caches.
- Compares source and destination manifests to estimate missing, changed, and already recovered files.
- Copies only what is needed, with resume state, structured logs, progress output, and conflict handling.
- Matches renamed folders with dossier scoring built from hashes, normalized names, archive families,
  binary/text signatures, size classes, and semantic source-code hints.
- Compares archive-heavy trees without extraction by using virtual archive members, payload families, depth,
  recursive overlap scores, and capped conflict buckets.
- Provides bounded binary similarity reports for rename-heavy binary payloads.
- Accepts `ndex rsync ...`, `ndex rclone ...`, and `ndex copy ...` compatibility frontends for familiar workflows.

## Why Not Just `rsync` Or `rclone`?

`rsync` and `rclone` are excellent transfer tools. Nightindex is for the layer above transfer: figuring out
what should be transferred when the trees are incomplete, renamed, partially corrupt, or assembled from
several backups.

Nightindex adds:
- A reusable SQLite manifest, so you can inspect and compare without repeatedly walking a failing source.
- Resume state and retry planning for copies that hit unreadable files or transient disconnects.
- Folder identity scoring when directories were renamed, renumbered, or restored under different parents.
- Archive and binary similarity reports for payloads where filenames and exact checksums are not enough.
- Merge planning with `prefer-newer`, `prefer-larger`, `keep-both`, and manual review queues.
- Report history, cache inspection, and NDJSON logs for long recovery sessions.

Use `rsync`/`rclone` when paths are trustworthy and the job is straightforward. Use Nightindex when you need
evidence, resumability, and reviewable decisions before moving more data.

## Good Use Cases

- Recovering a failing external drive onto a ZFS/Btrfs pool.
- Merging several old backups into one canonical dataset.
- Comparing renamed exploit, firmware, source, or research folders after numbering schemes changed.
- Copying everything that still reads while preserving provenance for manual review.
- Auditing what a partial copy already recovered before reconnecting a suspect disk.
- Finding likely duplicates or newer variants when checksums and paths disagree.
- Tracking repeated compare runs with stored report history.

## Workflow

1. Scan each source or destination into a SQLite manifest.
2. Review aggregate differences with `brief`, `compare-summary`, `dossier`, or archive/binary compare reports.
3. Generate a copy or merge plan.
4. Dry-run the operation.
5. Execute, monitor logs, and resume failed rows if needed.

## Build

```bash
cargo build --release
```

Binary output:
- `target/release/nightindex`
- `target/release/ndex`

`ndex` is the compact alias for the same CLI.

## Quick Start

Scan a source and destination:

```bash
nightindex scan \
  --root /mnt/recovery-source \
  --label source \
  --db /var/tmp/nightindex/source.sqlite \
  --exclude firmware-cache \
  --hash

nightindex scan \
  --root /srv/recovered-dataset \
  --label recovered \
  --db /var/tmp/nightindex/recovered.sqlite \
  --hash
```

Review the gap:

```bash
nightindex brief \
  --left-db /var/tmp/nightindex/source.sqlite \
  --right-db /var/tmp/nightindex/recovered.sqlite \
  --left source \
  --right recovered
```

Copy missing or changed files with a saved plan and NDJSON log:

```bash
nightindex sync \
  --left-db /var/tmp/nightindex/source.sqlite \
  --right-db /var/tmp/nightindex/recovered.sqlite \
  --left source \
  --right recovered \
  --from /mnt/recovery-source \
  --to /srv/recovered-dataset \
  --write-plan /var/tmp/nightindex/source-to-recovered-plan.json \
  --policy ./examples/recovery-policy.yaml \
  --progress-every 500 \
  --log /var/tmp/nightindex/source-to-recovered.ndjson
```

## Command Map

- `scan`: build or refresh a manifest.
- `brief`: compact copy estimate.
- `compare-summary`: aggregate manifest diff metrics.
- `dossier` / `intel`: renamed folder and project identity scoring.
- `extract-check` / `extcheck`: archive-family comparison.
- `archive-member-diff` / `amdiff`: virtual archive-member diff.
- `archive-member-plan` / `amplan`: read-only archive reconcile action planning.
- `archive-member-merge-plan` / `am2merge`: convert archive plan rows into a merge plan.
- `archive-recursive-compare` / `arcmp`: recursive archive overlap metrics.
- `binary-diff-summary` / `bdiff`: bounded binary similarity report.
- `plan-copy-missing` / `plan`: write a copy plan.
- `sync-copy-missing` / `sync`: plan and execute in one command.
- `execute-copy-missing` / `execute`: run a saved copy plan.
- `resume-plan` / `resume`: inspect, export, prune, or execute resume rows.
- `logs`: summarize copy NDJSON logs.
- `status`: summarize DB and recent run health.
- `inspect-cache`: inspect fingerprint/signature cache coverage.
- `report-history`: query persisted analysis reports.
- `merge-plan`: convert dossier action CSV to a merge plan.
- `merge-apply`: apply a merge plan.
- `copy`, `rsync`, `rclone`: compatibility copy frontends.

## Core Commands

### `scan`

Build or refresh a manifest. With `--hash`, file hashes become strong anchors for compare and dossier matching.

```bash
nightindex scan --root <path> --label <name> --db <manifest.sqlite> \
  [--exclude <prefix>] [--exclude-if-present <marker>] [--policy <policy.yaml|json>] [--hash]
```

### `brief`

Compact difference and copy estimate.

```bash
nightindex brief \
  --left-db <left.sqlite> \
  --right-db <right.sqlite> \
  --left <left_label> \
  --right <right_label> \
  [--out-json <file>] [--out-csv <file>]
```

### `compare-summary`

Aggregate manifest diff metrics with stable schema fields:

- `report_schema: "nightindex.compare_summary"`
- `report_version: 1`
- `cache_metrics.left_profile_cache`
- `cache_metrics.right_profile_cache`

```bash
nightindex compare-summary \
  --left-db <left.sqlite> \
  --right-db <right.sqlite> \
  --left <left_label> \
  --right <right_label> \
  [--out-json <file>] [--out-csv <file>]
```

### `sync` and `execute`

`sync` plans and copies in one command. `execute` runs a saved plan.

```bash
nightindex plan \
  --left-db <left.sqlite> \
  --right-db <right.sqlite> \
  --left <left_label> \
  --right <right_label> \
  [--policy <policy>] \
  --out-json /tmp/plan.json

nightindex execute \
  --plan /tmp/plan.json \
  --from <left_root> \
  --to <right_root> \
  [--overwrite] [--dry-run] [--stop-on-error] \
  [--progress-every <N>] \
  [--log /tmp/events.ndjson] \
  [--policy <policy>]
```

## Renamed Folder Matching

### `dossier` / `intel`

`dossier` scores likely folder matches when directory names, numbering schemes, or version suffixes changed.
It is read-only and can emit both review reports and action CSV files for merge planning.

Signals include:
- Exact names, stems, extensions, folder tokens, and hashes.
- Normalized file and parent-folder aliases.
- Binary/text/archive classification.
- Archive family and payload signatures.
- Virtual archive shape tokens and depth.
- Lightweight semantic source/text signatures.
- Size classes and binary descriptors.

Confidence tiers:
- `identical`: strong exact-name/hash anchors and high overlap.
- `similar`: normalized names, archive family, binaryity, or semantic signals align.
- `possible`: weaker folder, extension, family, or hash evidence.
- `manual`: low or conflicting evidence.

```bash
nightindex dossier \
  --left-db <left.sqlite> \
  --right-db <right.sqlite> \
  --left <left_label> \
  --right <right_label> \
  --top-k 15 \
  [--confidence <manual|possible|similar|identical>] \
  [--only-action <apply|review|manual>] \
  [--one-per-left] \
  [--policy <policy>] \
  [--out-json <file>] \
  [--out-csv <file>] \
  [--out-actions-csv <file>]
```

## Archive-Aware Reports

### `extcheck`

Compare archive-like payload families and extraction potential between two trees.

```bash
nightindex extcheck \
  --left-db <left.sqlite> \
  --right-db <right.sqlite> \
  --left <left_label> \
  --right <right_label> \
  [--out-json <file>] [--out-csv <file>]
```

### `archive-member-diff` / `amdiff`

Diff persisted virtual archive-member manifests.

```bash
nightindex archive-member-diff \
  --left-db <left.sqlite> \
  --right-db <right.sqlite> \
  --left <left_label> \
  --right <right_label> \
  [--nested-stats] \
  [--out-json <file>] [--out-csv <file>]
```

### `archive-member-plan` / `amplan`

Generate read-only archive reconcile rows from archive-member diff signals.

```bash
nightindex archive-member-plan \
  --left-db <left.sqlite> \
  --right-db <right.sqlite> \
  --left <left_label> \
  --right <right_label> \
  --out-json /tmp/archive-member-plan.json \
  --out-csv /tmp/archive-member-plan.csv
```

### `archive-member-merge-plan` / `am2merge`

Convert `archive-member-plan` JSON into a `merge-apply` plan.

```bash
nightindex archive-member-merge-plan \
  --archive-member-plan-json /tmp/archive-member-plan.json \
  --imports-root /srv/recovered-dataset/_imports \
  --canonical-root /srv/recovered-dataset \
  --policy prefer-newer \
  --out-json /tmp/archive-merge-plan.json \
  --out-csv /tmp/archive-merge-plan.csv
```

### `archive-recursive-compare` / `arcmp`

Compute recursive virtual archive overlap metrics without extracting archives to disk.

```bash
nightindex archive-recursive-compare \
  --left-db <left.sqlite> \
  --right-db <right.sqlite> \
  --left <left_label> \
  --right <right_label> \
  --max-bucket-items 2000 \
  [--db /var/tmp/nightindex/reports.sqlite] \
  [--tag nightly-arcmp] \
  [--out-json <file>] [--out-csv <file>]
```

Report fields include:
- `exact_overlap_score`
- `nested_overlap_score`
- `depth_weighted_overlap_score`
- `quality_band`
- `bucket_output_truncated`
- full conflict totals, even when bucket output is capped by `--max-bucket-items`

## Binary Similarity

### `binary-diff-summary` / `bdiff`

Compare two binary files with bounded sampling and compact digest output.

```bash
nightindex binary-diff-summary \
  --left-file /srv/recovered-dataset/_imports/payload-a.bin \
  --right-file /srv/recovered-dataset/canonical/payload-b.bin \
  [--window-size 4096] \
  [--max-windows 24] \
  [--db /var/tmp/nightindex/reports.sqlite] \
  [--tag nightly-diff] \
  [--out-json <file>]
```

Useful fields:
- `similarity`
- `aligned_similarity`
- `shifted_alignment_similarity`
- `content_similarity`
- `histogram_similarity`
- left/right full hashes and sizes

## Merge Workflow

Convert dossier or archive action rows into materialized merge plans, then dry-run and apply.

```bash
nightindex merge-plan \
  --actions-csv /var/tmp/nightindex/probable-renames.csv \
  --imports-root /srv/recovered-dataset/_imports \
  --canonical-root /srv/recovered-dataset \
  --policy prefer-newer \
  --out-json /tmp/merge-plan.json

nightindex merge-apply \
  --plan /tmp/merge-plan.json \
  --dry-run

nightindex merge-apply \
  --plan /tmp/merge-plan.json \
  --only-decision apply \
  --max-items 100
```

Policies:
- `prefer-newer`
- `prefer-larger`
- `keep-both`
- `manual`

`merge-plan` JSON includes:
- `summary.total_items`
- `summary.apply_items`
- `summary.keep_both_items`
- `summary.manual_items`

`merge-apply` summary includes:
- `planned_items`
- `selected_items`
- `skipped_by_filter`
- `skipped_by_limit`
- `conflicts_label`
- `conflicts_non_destructive`

## Resume, Logs, And Reports

### `resume`

Build retry plans from prior copy run state.

```bash
nightindex resume \
  --db <scan-or-copy.sqlite> \
  [--list-sessions] \
  [--stats --session-id <session_id>] \
  [--prune-completed --session-id <session_id> [--dry-run-prune] [--vacuum]] \
  [--jsonl-out /tmp/resume-items.jsonl] \
  [--session-id <session_id>] \
  [--only-failed] \
  [--max-attempts <N>] \
  [--out-json /tmp/resume-plan.json] \
  [--execute --from <left_root> --to <right_root>]
```

### `logs`

Summarize copy NDJSON logs.

```bash
nightindex logs \
  --file <copy.ndjson> \
  [--tail 200] \
  [--failures-only] \
  [--top-errors 5] \
  [--retry-jsonl-out /tmp/retry.jsonl]
```

### `status`

Summarize manifest and copy-run health.

```bash
nightindex status --db <manifest.sqlite> [--window-minutes 180]
```

### `inspect-cache`

Inspect fingerprint/signature cache coverage.

```bash
nightindex inspect-cache \
  --db <manifest.sqlite> \
  [--label <label>] \
  [--out-json <file>]
```

### `report-history`

Query persisted `binary-diff-summary` and `archive-recursive-compare` reports.

```bash
nightindex report-history \
  --db /var/tmp/nightindex/reports.sqlite \
  [--kind binary_diff_summary] \
  [--tag smoke] \
  [--left-ref /path/to/left.bin] \
  [--right-ref /path/to/right.bin] \
  [--min-score-primary 0.80] \
  [--max-score-primary 1.00] \
  [--sort created-desc|created-asc|score-primary-desc|score-primary-asc] \
  [--limit 50]
```

Output schema:
- `report_schema: "nightindex.report_history"`
- `report_version: 1`
- rows with `id`, `report_kind`, `tag`, `left_ref`, `right_ref`, `score_primary`, `score_secondary`, `created_at`

## Compatibility Frontends

`nightindex copy`, `nightindex rsync`, and `nightindex rclone` accept common transfer flags and run
Nightindex copy logic. Unsupported flags are reported to stderr.

```bash
ndex rsync --dry-run --delete-after --ignore-existing --stop-on-error /mnt/source /srv/destination
ndex rclone --checksum --include 'archives/**' --filter '- scratch/**' /mnt/source /srv/destination
ndex copy --progress-every 250 /mnt/source /srv/destination
```

Mapped options include:
- `-n`, `--dry-run`
- `--ignore-existing`, `--update`, `-u`
- `--checksum`, `-c`
- `--files-from <file>`
- `--exclude <pattern>`, `--exclude-from <file>`
- `--include <pattern>`, `--include-from <file>`
- `--filter <rule>`, `--filter-from <file>`
- `--exclude-if-present <name>`
- `--max-age <10m|2h|7d|seconds>`
- `--delete`, `--delete-before`, `--delete-during`, `--delete-after`, `--delete-excluded`
- `--copy-links`, `--copy-unsafe-links`, `--links`
- `--backup`, `--backup-dir <path>`
- `--log-file <path>`, `--log <path>`, `--policy <path>`
- `--progress-every <N>`
- `--size-only`, `--ignore-times`
- `--stats`, `--human-readable`, `--verbose`, `-v`

Accepted compatibility no-ops include `--perms`, `--times`, `--group`, `--owner`, `--chmod`,
`--progress`, and `--inplace`.

## Policy Format

`--policy` accepts YAML, JSON, or JSON5-style files.

```yaml
directory_prefixes:
  - firmware-cache
  - tmp/cache
folder_name_additions:
  - cache
  - Recovered Files
subtree_overrides:
  tmp:
    - .cache
    - node_modules
```

Fields:
- `directory_prefixes`: relative path prefixes to exclude.
- `folder_name_additions`: extra folder names treated as noise anywhere in the tree.
- `subtree_overrides`: extra noise folders only under a subtree prefix.

Default noise folders include common caches, recycle bins, VCS folders, temp folders, and recovered-file trash.

## Event Logs

Copy logs are NDJSON when `--log` is provided.

```json
{"schema_version":2,"rel_path":"projects/tooling/note.txt","action":"copy","existing_bytes":null,"bytes":1234,"dry_run":false,"overwrite":false,"reason":null}
{"schema_version":2,"rel_path":"archives/old.bin","action":"skip_conflict","existing_bytes":4096,"bytes":4096,"dry_run":false,"overwrite":false,"reason":"destination conflict: existing size 4096"}
{"schema_version":2,"rel_path":"firmware-cache/legacy.img","action":"source_missing","existing_bytes":null,"bytes":0,"dry_run":false,"overwrite":false,"reason":"source file missing"}
```

Common actions:
- `copy`
- `overwrite`
- `skip_existing`
- `skip_conflict`
- `source_missing`
- `fail`

## Release

Run the pre-release gate:

```bash
bash scripts/release_check.sh
```

Strict clippy mode:

```bash
NIGHTINDEX_STRICT_CLIPPY=1 bash scripts/release_check.sh
```

The default gate enforces:
- `cargo fmt --all -- --check`
- `cargo test --locked --all-targets`
- `cargo build --release --locked`

It also runs clippy in non-blocking mode unless strict mode is enabled.

Versioning:
- Bump `[package].version` in `Cargo.toml` before a tagged release.
- Keep `Cargo.lock` committed so `--locked` checks remain reproducible.

Benchmark archive compare on real manifests:

```bash
scripts/bench_archive_compare.sh <left.sqlite> <right.sqlite> <left_label> <right_label> [runs]
```

## Status

Nightindex v1 is functionally complete for recovery, compare, merge, and report workflows. The remaining
work is normal product hardening: more real-world fixtures, stricter lint cleanup, and expanded compatibility
coverage for uncommon `rsync`/`rclone` flags.
