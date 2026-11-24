# NIGHTINDEX V1 Status

## Current Status

This file records the original V1 CLI milestone for the compare, copy, merge and report workflows.
The [commercialisation dossier](COMMERCIALISATION.md) and [roadmap](ROADMAP.md) define the proposed product
work beyond that milestone. The [source baseline](docs/commercialisation/BASELINE_AND_SOURCES.md) records
implementation limits; the [feature register](docs/commercialisation/FEATURES.md) and
[validation plan](docs/commercialisation/VALIDATION.md) define additions and release acceptance.

Shipped:
- `nightindex` and `ndex` binaries.
- SQLite scanning with optional hashing, persistent fingerprint profiles, and signature cache reuse.
- Missing/changed file planning, execution, sync, resume, retry export, prune, and vacuum flows.
- Structured NDJSON copy logs and digestible console progress.
- `copy`, `rsync`, and `rclone` compatibility frontends for common transfer flags.
- Dossier matching for renamed or renumbered folder trees using hashes, normalized names, folder context,
  binary/text/archive signals, semantic hints, and archive shape evidence.
- Archive-aware compare commands: `extcheck`, `archive-member-diff`, `archive-member-plan`,
  `archive-member-merge-plan`, and `archive-recursive-compare`.
- Bounded binary similarity reports through `binary-diff-summary`.
- Merge planning and materialization with `prefer-newer`, `prefer-larger`, `keep-both`, and `manual` policies.
- Report persistence and `report-history` query/filter/sort UX.
- Release and benchmark helper scripts.

Historical validation recorded for V1:
- `cargo test -q` was recorded as passing 102 tests for both binaries.
- `bash scripts/release_check.sh` was recorded as passing in default mode.

The commercialisation review counted 103 test attributes statically in the pinned source. It did not rerun
tests or builds; the historical results above are not fresh validation of the proposed commercial features.

## Design Goals

- Prefer content and multi-file evidence over paths because naming and numbering schemes can drift.
- Preserve provenance under import roots until merge confidence is high.
- Treat caches, build outputs, temp folders, extracted trash, and editor artifacts as weak identity signals.
- Treat PoCs, patches, scripts, binaries, manifests, firmware metadata, and rare source files as strong identity signals.
- Keep copy operations observable with readable progress plus structured logs.
- Make expensive reports bounded by default while preserving full totals and scores.

## Remaining Hardening

1. Clean strict clippy lint debt so `NIGHTINDEX_STRICT_CLIPPY=1 bash scripts/release_check.sh` can become the default gate.
2. Add more real-world benchmark fixtures for huge archive-member manifests and mixed backup trees.
3. Expand compatibility handling for uncommon `rsync`/`rclone` flags only when they map cleanly to Nightindex semantics.
4. Add release packaging notes once version tags and artifacts are being produced regularly.

## Useful Commands

```bash
cargo test -q
bash scripts/release_check.sh
scripts/bench_archive_compare.sh <left.sqlite> <right.sqlite> <left_label> <right_label> [runs]
```
