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
Build or refresh a tree manifest.

```bash
nightindex scan --root <path> --label <name> --db <manifest.sqlite> \
  [--exclude <prefix>] [--policy <policy.yaml|json>] [--hash]
```

`compare-summary`  
Quick aggregate diff metrics for two manifests.

```bash
nightindex compare-summary \
  --left-db <left.sqlite> \
  --right-db <right.sqlite> \
  --left <left_label> \
  --right <right_label> \
  [--out-json <file>] [--out-csv <file>]
```

`brief`  
Compact aggregate diff with copy estimate.

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
folders and renumbered exploit buckets.

```bash
nightindex dossier \
  --left-db <left.sqlite> \
  --right-db <right.sqlite> \
  --left <left_label> \
  --right <right_label> \
  --top-k 15 \
  --out-json <file> \
  --out-csv <file> \
  [--policy <policy>]
```

`extcheck`  
Compare archive-like payload families and extraction potential between two trees.

```bash
nightindex extcheck \
  --left-db <left.sqlite> \
  --right-db <right.sqlite> \
  --left <left_label> \
  --right <right_label> \
  [--out-json <file>] [--out-csv <file>]
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

```bash
nightindex rsync \
  [rsync flags...] \
  <source> <destination>
  
nightindex rclone \
  [rclone flags...] \
  <source> <destination>
```

Common mapped options:

- `-n`, `--dry-run` → dry-run mode  
- `--ignore-existing`, `--update`, `-u` → skip existing files (no overwrite)  
- `--checksum`, `-c` → force hash-based file matching  
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
