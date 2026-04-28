# Nightindex Command Reference

This file documents the proposed final CLI behavior for `nightindex`.

## Core Commands

`scan`

Build or refresh a file manifest for one tree.

```bash
nightindex scan \
  --root /mnt/nvme1tb \
  --db /var/tmp/nightindex/nvme1tb.sqlite \
  --label nvme1tb \
  --exclude 03_FIRMWARE \
  --policy /home/user/.config/nightindex/policy.yaml
```

`plan`

Compare two manifests and emit a frozen transfer plan.

```bash
nightindex plan \
  --left-db /var/tmp/nightindex/nvme1tb.sqlite \
  --right-db /var/tmp/nightindex/tank.sqlite \
  --left nvme1tb \
  --right tank \
  --out-json /var/tmp/nightindex/plan.json
```

`execute`

Apply a previously-generated plan.

```bash
nightindex execute \
  --plan /var/tmp/nightindex/plan.json \
  --from /mnt/nvme1tb \
  --to /tank/BUGBOUNTY \
  --overwrite \
  --progress-every 500 \
  --log /var/tmp/nightindex/events/2026-04-28.ndjson
```

`sync`

Plan and execute in one command.

```bash
nightindex sync \
  --left-db /var/tmp/nightindex/nvme1tb.sqlite \
  --right-db /var/tmp/nightindex/tank.sqlite \
  --left nvme1tb \
  --right tank \
  --from /mnt/nvme1tb \
  --to /tank/BUGBOUNTY \
  --write-plan /var/tmp/nightindex/plan.json \
  --dry-run \
  --log /var/tmp/nightindex/events/2026-04-28.ndjson
```
`plan` and `sync` are aliases for `plan-copy-missing` and `sync-copy-missing`.

`dossier` (alias `intel`)

Compare folder signatures between two labels and emit top-k identity matches.
`dossier` is read-only and only reads from the two manifest databases.

```bash
nightindex dossier \
  --left-db /var/tmp/nightindex/nvme1tb.sqlite \
  --right-db /var/tmp/nightindex/tank.sqlite \
  --left nvme1tb \
  --right tank \
  --top-k 5 \
  --out-json /var/tmp/nightindex/dossier.json \
  --out-csv /var/tmp/nightindex/dossier.csv
```

Example JSON output:

```json
{
  "left_db":"/var/tmp/nightindex/nvme1tb.sqlite",
  "right_db":"/var/tmp/nightindex/tank.sqlite",
  "left_label":"nvme1tb",
  "right_label":"tank",
  "top_k":5,
  "left_folder_count":12,
  "right_folder_count":9,
  "candidates":[
    {
      "left_folder":"archives/2023",
      "right_folder":"archives-2023",
      "overlap_weight":12.7,
      "left_weight":18.25,
      "right_weight":14.55,
      "overlap_ratio":0.437,
      "shared_rel_file_count":8
    }
  ]
}
```

Example CSV output:

```text
left_folder,right_folder,overlap_weight,left_weight,right_weight,overlap_ratio,shared_rel_file_count
archives/2023,archives-2023,12.7000,18.2500,14.5500,0.437000,8
```

`scan` supports an optional `--policy` file to define ignore rules. The policy parser accepts
YAML files (`.yml`/`.yaml`) and JSON/JSON5-like files (`.json`/`.json5`).

Example YAML policy:

```yaml
exclude_prefixes:
  - "03_FIRMWARE"
  - "tmp/cache"
folder_name_additions:
  - "cache"
  - "lost+found"
subtree_overrides:
  "tmp/cache":
    - "node_modules"
    - ".cache"
```

Example JSON/JSON5-like policy:

```json
{
  "exclude_prefixes": ["03_FIRMWARE", "tmp/cache"],
  "folder_name_additions": ["cache", "lost+found"],
  "subtree_overrides": {
    "tmp/cache": ["node_modules", ".cache"]
  }
}
```

`--policy` is also available on `plan`/`sync`/`execute` pipelines so the same per-subtree exclusions
can be applied to candidate generation and copy execution.

`exclude_prefixes` applies to relative path prefixes.
`folder_name_additions` applies to matching folder names anywhere in a subtree when a folder is in the
default noise set.

Default noise folders:

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
- `Node_modules`

If `--policy` is omitted, policy-based filtering is disabled except for explicit `--exclude` prefixes.
CLI default using `--exclude` only.

## Optional Utilities

- `compare-summary`: prints and optionally writes aggregate diff metrics.
- `dossier`/`intel`: computes per-folder identity scores and outputs top-k folder rename candidates.
## Event Log

Execution logging uses NDJSON. With `--log`, one JSON object is written per line:

```json
{"schema_version":2,"rel_path":"03_FIRMWARE/akita_dump/vendor_boot_a.img","action":"copy","existing_bytes":null,"bytes":12345,"dry_run":false,"overwrite":false,"reason":null}
{"schema_version":2,"rel_path":"03_FIRMWARE/legacy.bin","action":"skip_conflict","existing_bytes":4096,"bytes":4096,"dry_run":false,"overwrite":false,"reason":"destination conflict: existing size 4096"}
{"schema_version":2,"rel_path":"03_FIRMWARE/missing.bin","action":"source_missing","existing_bytes":null,"bytes":0,"dry_run":false,"overwrite":false,"reason":"missing: /mnt/nvme1tb/03_FIRMWARE/missing.bin"}
```

`action` is one of:
`source_missing`, `skip_existing`, `skip_conflict`, `copy`, `overwrite`, `fail`.

`bytes` is the bytes copied for `copy`/`overwrite`, destination bytes for conflict events, else `0`.

`reason` is optional and should describe the condition for non-success actions.

Every `execute`/`sync` run should still print a final summary JSON object to stdout
with counts such as `planned_files`, `copied_files`, `skipped_existing`,
`skipped_conflict`, `failed_files`, and `copied_bytes`.
