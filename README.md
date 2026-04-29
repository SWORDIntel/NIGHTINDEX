# Nightindex

Nightindex is an indexed, policy-aware replacement workflow for damaged file trees.
It is designed for rescue/recovery jobs where directory trees are noisy, renamed, and partially
corrupted but file content can still be matched reliably.

The process is:
1. Scan each side into a SQLite manifest.
2. Diff/manually compare manifests.
3. Execute a filtered copy plan.

## Build

```bash
cargo build --release
```

Binary output: `target/release/nightindex`.
The same binary is also installed as `target/release/ndex` (executable alias only).

Alias map:
- `nightindex` is the full command name.
- `ndex` is the executable alias for `nightindex`.
- `dossier` also has the `intel` alias.
- `plan-copy-missing` also has the `plan` alias.
- `sync-copy-missing` also has the `sync` alias.
- `execute-copy-missing` also has the `execute` alias.
- `extract-check` also has the `extcheck` alias.

## Progress Snapshot (2026-04-29)

Implemented and shipping on the active branch:
- Resume workflow: `resume`/`resume-plan`, direct execute, session listing/stats, filtered retries,
  prune/vacuum maintenance, JSONL export.
- Dossier workflow: confidence tiers, action filtering, archive-signal coverage metrics.
- Compatibility workflow: `ndex rsync ...` and `ndex rclone ...` frontends with mapped flag support.
- Ops visibility: `status` command for DB/run health summary, richer `logs` error-class summaries.
- Merge workflow: `merge-plan` + hardened `merge-apply` with recursive directory materialization,
  deterministic `keep-both` naming, and richer apply summary counters.
- Persistent cache v2: cross-label file fingerprint profile cache in SQLite with size/mtime/hash
  invalidation, `status` visibility, and cached binary/text/archive signature fields for dossier
  matching.
- Archive-recursive foundation: `extract-check` reports virtual archive paths/families/depth, and
  dossier scoring consumes virtual archive shape tokens.
- Semantic text signatures: scan enriches text-like files with lightweight import/function/key
  signals that become stable `TEXTSIG` evidence for dossier ranking.

In progress:
- Archive-recursive indexing without extraction and semantic code/text signatures.
- Extend cached signatures from lightweight semantic signals into deeper content-derived
  binary/text/archive descriptors.

## Recommended recovery use-case

Example for your scenario: copy from NVMe backup source to `/tank/btrfs-recovery/BUGBOUNTY`, while
keeping firmware folders out of the initial pass.

```bash
nightindex scan \
  --root /media/john/DSMIL1 \
  --label dsmil1 \
  --db /tank/nightindex/dsmil1.sqlite \
  --exclude 03_FIRMWARE \
  --hash

nightindex scan \
  --root /tank/btrfs-recovery/BUGBOUNTY \
  --label tank \
  --db /tank/nightindex/tank.sqlite \
  --hash

nightindex brief \
  --left-db /tank/nightindex/dsmil1.sqlite \
  --right-db /tank/nightindex/tank.sqlite \
  --left dsmil1 \
  --right tank

nightindex sync \
  --left-db /tank/nightindex/dsmil1.sqlite \
  --right-db /tank/nightindex/tank.sqlite \
  --left dsmil1 \
  --right tank \
  --from /media/john/DSMIL1 \
  --to /tank/btrfs-recovery/BUGBOUNTY \
  --write-plan /tank/nightindex/plan-dsmil1-tank.json \
  --policy /home/john/.config/nightindex/policy.yaml \
  --progress-every 500 \
  --log /tank/nightindex/sync-dsmil1-tank.ndjson
```

## Commands

`scan`  
Build or refresh a tree manifest. Prints a short stderr summary with file, symlink, hash, reuse, and exclude counts.

```bash
nightindex scan --root <path> --label <name> --db <manifest.sqlite> \
  [--exclude <prefix>] [--policy <policy.yaml|json>] [--hash]
```

`compare-summary`  
Quick aggregate diff metrics for two manifests.
JSON output includes `report_schema: "nightindex.compare_summary"`, `report_version: 1`,
and stable cache counters under `cache_metrics.left_profile_cache` /
`cache_metrics.right_profile_cache` (`hits`, `misses`).

```bash
nightindex compare-summary \
  --left-db <left.sqlite> \
  --right-db <right.sqlite> \
  --left <left_label> \
  --right <right_label> \
  [--out-json <file>] [--out-csv <file>]
```

`brief`  
Compact aggregate diff with copy estimate. Prints JSON plus a short stderr summary with copy counts and ETA when available.

```bash
nightindex brief \
  --left-db <left.sqlite> \
  --right-db <right.sqlite> \
  --left <left_label> \
  --right <right_label> \
  [--out-json <file>] [--out-csv <file>]
```

`dossier` (alias: `intel`)  
Read-only folder identity scoring between two manifests. Useful for matching renamed
folders and renumbered exploit buckets where paths drift but semantic content stays related.

`dossier` now combines legacy dossier signals with normalized fingerprint signals:
- `N:` exact file names, `S:` stems, `E:` extensions, `ES:` archive-aware extension stems, `H:` hashes, and folder tokens.
- `NF:` normalized file-name aliases from `file_fingerprints`.
- `NFP:` normalized parent-folder aliases and prefix variants.
- `BIN` / `TEXT`: binaryity class.
- `ARCH:<family>` and `ARCHFAM:<family>`: archive family grouping.
- `ARCHSIG:<payload>`: archive payload signature for related compressions (`tar` for both `tar.gz` and `tar.xz`).
- `ARCHPAY`, `ARCHVIRT`, and `ARCHDEPTH`: cached payload/virtual-path archive signals used before extraction.

Confidence tiers are interpreted as follows:

`identical`
- Multiple `N:` exact-name overlaps plus at least one `H:` file hash overlap.
- High overlap ratio and strong matching anchor counts.

`similar`
- `NF:` and `NFP:` signal clusters match while exact names diverge.
- Archive family (`ARCH`, `ARCHFAM`) and binaryity (`BIN`, `TEXT`) alignment.

`possible`
- Folder structure, extension/family overlap, or hash overlap without stronger anchors.

`manual`
- Weak or conflicting evidence only.

Compatibility expectation
- If a database lacks `file_fingerprints` rows, `dossier` gracefully falls back to legacy scoring.
- Output remains read-only JSON/CSV and does not mutate the source/destination databases.
- Dossier JSON includes explicit report metadata fields:
  `report_schema: "nightindex.dossier"` and `report_version: 1`.
- Cache counters are surfaced in stable nested fields:
  `cache_metrics.left_profile_cache` / `cache_metrics.right_profile_cache` (`hits`, `misses`).
  Legacy top-level `left_profile_cache` / `right_profile_cache` fields are still emitted.

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
  --out-json <file> \
  --out-csv <file> \
  [--out-actions-csv <file>] \
  [--policy <policy>]
```

`extcheck`  
Compare archive-like payload families and extraction potential between two trees. Prints JSON plus
a short stderr summary of exact and stem matches. Archive entries include virtual paths such as
`qcom_payload/@tar/gz`, archive family, payload signature, and nested archive depth.

```bash
nightindex extcheck \
  --left-db <left.sqlite> \
  --right-db <right.sqlite> \
  --left <left_label> \
  --right <right_label> \
  [--out-json <file>] [--out-csv <file>]
```

`archive-member-diff` (alias: `amdiff`)  
Diff persisted virtual archive-member manifests between two labels/DBs.

```bash
nightindex archive-member-diff \
  --left-db <left.sqlite> \
  --right-db <right.sqlite> \
  --left <left_label> \
  --right <right_label> \
  [--out-json <file>] [--out-csv <file>]
```

`archive-member-merge-plan` (alias: `am2merge`)  
Bridge archive-member matches into merge actions/plan inputs for `merge-plan` and `merge-apply`.

```bash
nightindex archive-member-merge-plan \
  --left-db <left.sqlite> \
  --right-db <right.sqlite> \
  --left <left_label> \
  --right <right_label> \
  --out-actions-csv /tmp/archive-actions.csv \
  [--confidence <manual|possible|similar|identical>] \
  [--one-per-left]
```

Practical examples:

```bash
# 1) Generate archive-driven action candidates (review first)
ndex am2merge \
  --left-db /tank/nightindex/src.sqlite \
  --right-db /tank/nightindex/dst.sqlite \
  --left src \
  --right dst \
  --confidence similar \
  --one-per-left \
  --out-actions-csv /tank/nightindex/archive-actions.csv

# 2) Convert actions to an executable merge plan
ndex merge-plan \
  --actions-csv /tank/nightindex/archive-actions.csv \
  --imports-root /tank/recovery/_imports \
  --canonical-root /tank/recovery \
  --policy prefer-newer \
  --out-json /tank/nightindex/archive-merge-plan.json

# 3) Apply with dry-run, then execute
ndex merge-apply --plan /tank/nightindex/archive-merge-plan.json --dry-run
ndex merge-apply --plan /tank/nightindex/archive-merge-plan.json
```

`logs`  
Summarize NDJSON copy logs produced by `--log` during execute/sync/compat copy runs.

```bash
nightindex logs \
  --file <copy.ndjson> \
  [--tail 200] \
  [--failures-only]
```

`inspect-cache`  
Read-only per-label cache and signature-density report.

```bash
nightindex inspect-cache \
  --db <manifest.sqlite> \
  [--label <label>] \
  [--out-json <file>]
```

`resume` (alias for `resume-plan`)  
Build a retry plan from resume-state rows stored during prior copy runs.

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

`plan-copy-missing` (alias: `plan`)  
Generate a deterministic copy plan for missing or changed files.

```bash
nightindex plan \
  --left-db <left.sqlite> \
  --right-db <right.sqlite> \
  --left <left_label> \
  --right <right_label> \
  [--policy <policy>] \
  --out-json /tmp/plan.json
```

`execute`  
Execute a saved plan.

```bash
nightindex execute \
  --plan /tmp/plan.json \
  --from <left_root> \
  --to <right_root> \
  [--overwrite] [--dry-run] [--stop-on-error] \
  [--progress-every <N>] \
  [--log /tmp/events.ndjson] \
  [--policy <policy>]
```

`sync-copy-missing` (alias: `sync`)  
Plan and execute in one command.

```bash
nightindex sync \
  --left-db <left.sqlite> \
  --right-db <right.sqlite> \
  --left <left_label> \
  --right <right_label> \
  --from <left_root> \
  --to <right_root> \
  [--overwrite] [--dry-run] [--stop-on-error] \
  --write-plan <file> \
  [--policy <policy>] \
  [--log /tmp/events.ndjson]
```

`rclone` and `rsync`  
Compatibility frontends that accept common transfer flags and execute the same copy plan logic.
Mapped and accepted options are applied; unsupported ones are reported to stderr.
Positional source and destination arguments are treated as roots; a trailing slash is accepted but
does not switch the command into a separate "copy contents" mode.

```bash
nightindex rsync \
  [rsync flags...] \
  <source> <destination>
  
nightindex rclone \
  [rclone flags...] \
  <source> <destination>
```

`merge-plan` and `merge-apply`  
Materialize merge actions from dossier action CSV into an executable merge plan.

```bash
nightindex merge-plan \
  --actions-csv /tank/nightindex/probable_renamed_actions.csv \
  --imports-root /tank/btrfs-recovery/BUGBOUNTY/_imports \
  --canonical-root /tank/btrfs-recovery/BUGBOUNTY \
  --policy prefer-newer \
  --out-json /tmp/merge-plan.json

nightindex merge-apply \
  --plan /tmp/merge-plan.json \
  [--dry-run]
```

Common mapped options:

- `-n`, `--dry-run` → dry-run mode  
- `--ignore-existing`, `--update`, `-u` → skip existing files (no overwrite)  
- `--checksum`, `-c` → force hash-based file matching  
- `--files-from <file>` → allowlist relative paths from a newline-delimited file  
- `--exclude-if-present <name>` → skip any directory containing the named marker file  
- `--stop-on-error` → fail fast on the first copy error  
- `--exclude <pattern>` and `--exclude-from <file>` → import excludes into scan policy  
- `--delete`, `--delete-before`, `--delete-during`, `--delete-after` → delete destination-only files  
- `--delete-excluded` → delete destination files matched by exclude policy as well
- `--include <pattern>`, `--include-from <file>` → include allowlist patterns  
- `--filter <rule>`, `--filter-from <file>` → `+` rules add allowlist patterns and `-` rules add blocklist patterns  
- `--log-file <path>`, `--log <path>` and `--policy <path>`  
- `--progress-every <n>` → override progress interval  
- `--size-only`, `--ignore-times` → recovery mode: treat same-size files as equivalent when destination
  conflict checks are made (helps when timestamps/mtime drift is common)
- `--max-age <age>` → exclude files older than the specified age; accepts seconds or suffixes like
  `10m`, `2h`, or `7d`
- `--copy-links`, `--copy-unsafe-links` → dereference symlinks and copy target contents
- `--links` → preserve symlinks
- `--backup`, `--backup-dir <path>` → back up overwritten or deleted destination entries before replacement
  (`--backup` uses a local `.nightindex-backup` tree under the destination root)
- `--stats`, `--human-readable`, `-h`, `--verbose`, `-v` → accepted compatibility flags that add stderr notes
- Accepted and ignored: `--perms`, `--times`, `--group`, `--owner`, `--chmod`, `--progress`
- Accepted compat flag: `--inplace`
- Still unsupported in direct compat mode: `--rsh`, `--ssh`, `--dry-run-mode`

Usage examples:

```bash
ndex rsync --dry-run --delete-after --ignore-existing --stop-on-error --progress-every 250 /mnt/source /tank/dest
ndex rclone --checksum --include 'QCOM/**' --filter '- QCOM/tmp/**' /mnt/source /tank/dest
```

Terminal shortcuts:

- `nightindex` full binary name  
- `ndex` compact alias for quick calls

`ndex` and `nightindex` are built from the same binary; use either in scripts.

## Policy format

`--policy` accepts YAML (`.yml/.yaml`), JSON, or JSON5-like files.

```yaml
directory_prefixes:
  - 03_FIRMWARE
  - tmp/cache
folder_name_additions:
  - cache
  - Recovered Files
subtree_overrides:
  tmp:
    - .cache
    - node_modules
```

`directory_prefixes` (a.k.a. `paths`, `prefix`, `exclude_prefixes`) are relative path prefixes.

`folder_name_additions` (a.k.a. `tokens`, `noise_dirs`, `folder_overrides`, `ignore_tokens`)
extends the noise-folder set anywhere in tree.

`subtree_overrides` (a.k.a. `overrides`, `subtree_rules`) adds noise folders only when traversal is
already inside that subtree prefix.

### Default noise folders

- `.cache`
- `.DS_Store`
- `.git`
- `.nobackup`
- `.svn`
- `.Trash`
- `.Trashes`
- `$RECYCLE.BIN`
- `$RECYCLE`
- `Desktop.ini`
- `Recovered Files`
- `System Volume Information`
- `Temporary Items`
- `Thumbs.db`
- `tmp`
- `temp`

## Event logs

Execution logging uses NDJSON (`schema_version:2`) when `--log` is provided:

```json
{"schema_version":2,"rel_path":"BUGBOUNTY/01_EXPLOITS/note.txt","action":"copy","existing_bytes":null,"bytes":1234,"dry_run":false,"overwrite":false,"reason":null}
{"schema_version":2,"rel_path":"BUGBOUNTY/old.bin","action":"skip_conflict","existing_bytes":4096,"bytes":4096,"dry_run":false,"overwrite":false,"reason":"destination conflict: existing size 4096"}
{"schema_version":2,"rel_path":"03_FIRMWARE/legacy.img","action":"source_missing","existing_bytes":null,"bytes":0,"dry_run":false,"overwrite":false,"reason":"source file missing"}
```

`action` values:

- `source_missing`
- `skip_existing`
- `skip_conflict`
- `copy`
- `overwrite`
- `fail`

Each execute/sync run prints one final summary JSON object with:

- `mode`
- `dry_run`
- `overwrite`
- `planned_files`
- `copied_files`
- `skipped_existing`
- `skipped_conflict`
- `overwritten_files`
- `missing_source`
- `failed_files`
- `copied_bytes`

## Roadmap (next phases)

- Binary diffing: add fast, content-derived binary descriptors and delta-oriented comparison signals to improve same-family file decisions where names/hashes drift.
- Archive-recursive compare: extend archive-aware matching from member-level diff into recursive cross-archive compare/reporting for nested payload trees.
- Persistent fingerprint DB expansion: broaden cached fingerprint schema and reuse coverage (binary/text/archive signals, richer invalidation/stats) for faster re-scans and stronger dossier/merge confidence.
