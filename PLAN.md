# NIGHTINDEX Implementation Plan

## Current Status

Estimated total progress: about 91%.

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
- Current test baseline: `cargo test -q` passes 58 tests for both binaries.
- Deep dossier scoring now uses normalized fingerprint profiles for renamed-folder/file matching (suffix-noise robust: `final`, `v2`, `old`, `copy`, date-ish).
- Resume, logs/status, and merge-apply hardening are implemented and pushed.
- Persistent cache v1 is implemented: scan-time file fingerprint profiles are cached in SQLite by
  path/type/size/mtime/hash input, reused across labels, invalidated by changed file metadata, and
  exposed through `status` as `signature_cache_rows`.
- Persistent cache v2 is underway and usable: file fingerprint profiles now include cached
  binary/text/archive signature fields, persisted in `file_fingerprints`, migrated onto older DBs,
  and consumed as dossier evidence tokens.
- Archive-recursive foundation is implemented: `extract-check` now emits virtual archive path,
  family, payload signature, and depth metadata; dossier uses archive virtual path/depth tokens.
- Semantic text signatures are implemented in scan: lightweight import/function/key/section tokens
  are extracted for text-like files and persisted in `text_signature` as `TEXTSIG` dossier evidence.

## Next Highest-Value Work

1. Extend persistent fingerprint cache.
   Add content-derived binary/text/archive descriptors and report hit/miss counts in compare/dossier output.

2. Archive recursive indexing.
   Extend virtual archive metadata into true nested member manifests without extracting everything first.

3. Semantic source parsing.
   Add language-aware signatures for C, Rust, Python, Markdown, JSON, shell, and project metadata.

4. Binary similarity (NOT_STISLA-like improvements).
   Add archive/block-aware binary descriptors plus approximate matching for renamed large payloads.

5. Deep dossier tuning.
   Refine deep-dossier normalization thresholds and suffix-noise handling for rename-heavy datasets.

6. Merge apply polish.
   Add optional materialized manifest output and a conflict review CSV for manual decisions.

7. Parallel copy engine.
   Add bounded workers with per-disk throttles so USB2 and ZFS jobs stay sane.

8. Deeper rsync/rclone compatibility.
    Map more flags accurately, especially trailing slash behavior, partial-dir, backup suffixes, and checksum choices.

## Implementation Order

Immediate next step after reboot:
1. Extend `signature_cache` to store deeper `binary_signature`, `text_signature`, and
   `archive_signature` records with cache hit/miss counters in dossier/compare output.

Then:
2. Add archive member indexing using normalized virtual paths.
3. Add semantic source parsing and feed those tokens into dossier scoring.
4. Tune deep dossier normalization thresholds and suffix-noise coverage.
5. Add binary similarity descriptors and confidence boosts.
6. Add bounded parallel copy after the matching/index layers settle.

## Design Notes

- Keep paths as weak evidence because folder numbering and naming changed.
- Prefer checksum and multi-file anchors over exact path matching.
- Preserve provenance under `_imports` until merge confidence is high.
- Treat build outputs, caches, extracted trash, and transient files as weak identity signals.
- Treat PoCs, patches, scripts, binaries, manifests, firmware metadata, and rare source files as strong identity signals.
- All long-running copy operations should produce readable console progress and structured logs.
