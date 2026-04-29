# NIGHTINDEX Implementation Plan

## Current Status

Estimated total progress: about 82%.

Done:
- Core `nightindex` / `ndex` binaries and command aliases.
- SQLite scan, brief, dossier, extract-check, copy-plan, execute, and sync workflow.
- Baseline rsync/rclone compatibility frontend.
- Include, exclude, filter, max-age, delete, and symlink compatibility handling.
- Safer copy execution with temp staging and race-safe finalization.
- Digestible console summaries and structured copy progress logs.
- Compatibility support for `--files-from`, `--exclude-if-present`, `--backup`, `--backup-dir`, `--stats`, `--human-readable`, `--verbose`, and `-v`.
- README coverage for current CLI behavior.
- Dossier documentation updated with normalized fingerprint matching signals, confidence-tier interpretation, and fallback/compatibility expectations.
- Archive-aware dossier matching added via `ARCH:`/`ARCHFAM:` and payload signature (`ARCHSIG:`), enabling multi-part archive-family matching without extraction.
- NOT_STISLA-like dossier hardening: binaryity, archive-family, and size-class evidence now participate in dossier tie-breaks.
- Current test baseline: `cargo test -q` passes 29 tests for both binaries.
- Deep dossier scoring now uses normalized fingerprint profiles for renamed-folder/file matching (suffix-noise robust: `final`, `v2`, `old`, `copy`, date-ish).

## Next Highest-Value Work

1. Persistent fingerprint cache.
   Avoid rehashing and reparsing large trees across repeated scans.

2. Resume database.
   Keep durable per-run copy state instead of relying only on destination file existence.

3. Real merge materialization.
   Generate merge plans from `_imports` into canonical trees using keep-best, newer, larger, and manual-review rules.

4. Archive recursive indexing.
   Inspect nested archives as virtual trees without needing full extraction first.

5. Deep dossier tuning.
   Refine deep-dossier normalization thresholds and suffix-noise handling for rename-heavy datasets.

6. NDJSON log viewer.
   Summarize copy logs into human status, failures, ETA, retry lists, and speed history.

7. Parallel copy engine.
   Add bounded workers with per-disk throttles so USB2 and ZFS jobs stay sane.

8. Semantic source parsing.
   Add language-aware signatures for C, Rust, Python, Markdown, JSON, and shell.

9. Binary similarity (NOT_STISLA-like improvements).
   Add archive/block-aware binary descriptors plus approximate matching for renamed large payloads (size/entropy/rolling-hash blocks) and then integrate confidence boosts into dossier tiering.

10. Deeper rsync/rclone compatibility.
    Map more flags accurately, especially trailing slash behavior, partial-dir, backup suffixes, and checksum choices.

## Implementation Order

Immediate next step:
1. Build persistent fingerprint cache and resume database together.

Then:
2. Add the NDJSON log viewer, because it will validate copy/resume behavior.
3. Add merge materialization from `_imports` to canonical trees.
4. Add archive recursive indexing.
5. Tune deep dossier normalization thresholds and suffix-noise coverage (rename-robust matching).
6. Add binary similarity and semantic source parsing.
7. Add bounded parallel copy after state tracking is durable.

## Design Notes

- Keep paths as weak evidence because folder numbering and naming changed.
- Prefer checksum and multi-file anchors over exact path matching.
- Preserve provenance under `_imports` until merge confidence is high.
- Treat build outputs, caches, extracted trash, and transient files as weak identity signals.
- Treat PoCs, patches, scripts, binaries, manifests, firmware metadata, and rare source files as strong identity signals.
- All long-running copy operations should produce readable console progress and structured logs.
