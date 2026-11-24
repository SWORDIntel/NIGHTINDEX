# Repository baseline and source register

Prepared: **24 November 2026**. Status: source-backed planning baseline, not a runtime certification.

[Commercialisation dossier](../../COMMERCIALISATION.md) · [Roadmap](../../ROADMAP.md) · [Product specification](PRODUCT_SPEC.md) · [Feature catalogue](FEATURES.md) · [Validation plan](VALIDATION.md)

## 1. Scope and evidence standard

The inspected revision is [`SWORDIntel/NIGHTINDEX` at `d3e8e50684948bd81e01615acc74fd7448519743`](https://github.com/SWORDIntel/NIGHTINDEX/tree/d3e8e50684948bd81e01615acc74fd7448519743), the default-branch head retrieved for this dossier. Findings below refer to that immutable revision, not to later branches or unshared work. The repository metadata reported public visibility when checked on 24 November 2026. This documentation addition does not change repository visibility or licensing.

The fetched recursive tree contains the README, PLAN, Cargo manifest/lockfile, two shell scripts and four Rust source files. No repository `AGENTS.md` or `LICENSE` file appears in that tree; `Cargo.toml` has no `license` field. This establishes an unresolved repository licensing declaration, not a legal conclusion about ownership or third-party rights. The owner's explicit instructions govern this documentation work. [R1, R2]

Evidence labels in this dossier have distinct meanings:

- **Observed:** directly established by the inspected source or retrieved repository metadata.
- **Documented:** stated by an existing project document; not independently reproduced here.
- **Proposed:** product behaviour, acceptance criterion or commercial hypothesis to be implemented or tested.
- **Unknown:** not established by the inspected material. Absence from this snapshot does not establish absence from other projects or private development.

Cargo was not available in the working environment. No application build, runtime test, benchmark, device scan, copy, imaging operation or recovery was executed for this baseline. Documentation checks on this proposed addition are separate from validation of the application.

## 2. What can be built on

NIGHTINDEX has a useful Rust/SQLite CLI foundation: file manifests, optional full-file hashing, persistent fingerprints, multi-signal folder matching, comparison reports, selective copy plans, progress/events, per-file resume records, merge decisions and analysis-report history. The existing commands provide a credible starting point for a storage reconciliation product. They do not yet implement the complete historical catalogue and recovery workbench proposed in this dossier.

| Capability | Observed implementation | Material limit and roadmap consequence |
|---|---|---|
| CLI and storage | `nightindex` and `ndex` share `src/main.rs`. Package version is `0.1.0`, edition 2024; dependencies include SQLite, BLAKE3, Clap, Serde and WalkDir. [R2] | Preserve the CLI and extract reusable engine boundaries incrementally. A second binary is not a separate engine. No database replacement is justified by this inspection alone. |
| Inventory schema | `files` and `file_fingerprints` use `(label, rel_path)` primary keys and carry `scanned_at`. [R3] | These are mutable path rows. The schema has no scan-generation, coverage, media-identity or immutable file-observation model. Those are prerequisites for trustworthy historical loss accounting. |
| Rescan behaviour | Observed files are upserted in place; `load_label` returns all rows for the label without an age/generation filter. [R4] | Earlier versions are overwritten, while unobserved ordinary-file rows remain without an explicit current/history distinction. Surviving old rows are useful evidence, but this is not an existing loss ledger. |
| Partial scans | Walk/stat/read-link errors can be counted and printed while traversal continues; the command can return success after those errors. Hash or semantic-read failures can abort after previous rows were written. [R5] | Persist scan completion, exclusions, failed subtrees and checkpoints. A process exit code alone cannot substantiate complete coverage or absence. |
| Regular-file hashes | `blake3_file` reads the whole regular file. Prior hashes may be reused when type, size and mtime match. For symlinks, `fast_hash` holds target text. [R6] | Separate digest algorithm/scope, original capture time, cache reuse and symlink target representation. A new scan timestamp does not establish a fresh content verification. |
| Scan read behaviour | Fingerprint construction can sample binary bytes and read recognised small text/source files even without `--hash`. [R7] | Implement a distinct metadata-only profile with explicit I/O budgets. Do not describe the current no-hash mode as performing no content reads or as suitable by default for failing disks. |
| Folder matching | Dossier scoring combines names, hashes, extensions, folder context, language and other signals. Its `Identical` tier can follow high overlap with a small number of shared hashes. [R8] | Preserve candidate ranking, but do not interpret a heuristic folder tier as proof that every byte in two trees is identical. Exact content resolution requires its own evidence rule. |
| Archive-related comparison | `persist_virtual_archive_member` and related helpers derive records from archive-like paths, stems, suffixes and inferred families. The inspected functions do not open archives or enumerate their internal entries. [R9] | Current capability is archive-name/family/shape comparison. Parser-backed member cataloguing, encrypted/truncated states and nested-content hashing are new features. No archive subprocess or parser dependency was found in this source set. |
| Archive row retention | Rows from prior virtual-archive scans are deleted at scan completion, including when traversal errors were counted. [R5] | Bring archive observations under the same historical/coverage rules as ordinary files. Partial scans must not erase prior evidence. |
| Binary similarity | Sample-window, alignment, size and histogram signals exist. The `full_hash` in `binary_diff` uses FNV-1a64, not BLAKE3. Histogram and whole-file digest work read beyond the sampled windows. [R10] | Label similarity and digest algorithms accurately. Bounded sample counts do not mean bounded total file I/O. This report is not cryptographic verification of recovery. |
| Copy and resume | Plans, dry runs, events, progress and per-file status/attempt records exist. Regular copying stages a temporary file and renames it. [R11] | Retain these foundations; introduce media-bound immutable plans, source-change detection and separate copied/verified states. Resume reconstructs items from current manifest rows, so plan/version binding needs explicit design. |
| Copy correctness | The inspected regular-copy path uses `fs::copy` then rename, without post-copy read-back/digest verification or explicit `sync_all`. Matching size and mtime can cause a destination to be skipped before hash comparison. [R12] | Add a defined verification and durability contract. The finaliser's destination check followed by rename does not establish an atomic no-replace guarantee. Validate competing writers and crash boundaries. |
| Merge materialisation | Directory merge uses a separate direct `fs::copy` walk; `filter_map(Result::ok)` drops traversal failures and nonregular entries are skipped. [R13] | Unify execution guarantees across merge and selective-copy paths. Do not advertise every operation as staged, complete, resumable or equally preserving. |
| Preservation | README explicitly identifies permissions/times/group/owner/chmod compatibility flags as no-ops. Some symlink copying exists; creation is Unix-specific in this implementation. [R14] | Publish a preservation matrix for each supported OS/filesystem pair. Unsupported guarantees must be visible before execution, especially for ACLs, xattrs, hardlinks, sparse files and platform-specific streams. |
| Reports and history | Analysis reports retain time, references, tags, scores and JSON; `report-history` retrieves them. [R15] | Reuse this reporting pattern. Analysis-report history is distinct from historical file/version observations and a persistent loss ledger. |
| Names and scale | Path conversion uses `to_string_lossy`. Manifest loading and comparison collect rows in vectors/maps. Archive comparison bounds output buckets and improves ancestor lookup, while still materialising input rows. [R16] | Add lossless path storage, safe display/collision handling, pagination and measured scale limits. A capped report does not prove bounded total memory or arbitrary collection size. |

The source tree contains no GUI assets/dependencies, physical-media register, serial/WWN identity model, raw image reader, ddrescue adapter or RAID/ZFS topology/reconstruction backend. Those are proposed additions. `src/main.rs` is 491,524 bytes and 13,933 lines, including its test module: a reason to separate coherent engine components as they change, not a requirement for a wholesale rewrite. [R1, R2]

## 3. Validation actually evidenced

Static inspection found **103 `#[test]` attributes**: 89 in `main.rs`, six in `archive_compare.rs`, three in `binary_diff.rs` and five in `copy_exec.rs`. The two executable targets share these sources. This count is not a claim that the tests pass or compile on every platform.

`PLAN.md` records a historical baseline of 102 passing tests for both binaries and a passing default release check. That statement is **documented**, not reproduced for this dossier; the fetched source contains a later invalid-UTF-8 signature test. Do not promote the old statement into new test evidence. [R17]

The existing release script enforces formatting, locked tests and release build. Clippy is nonblocking unless strict mode is requested. It checks that a release `nightindex` executable exists. Locked resolution is useful, but does not alone demonstrate bit-for-bit reproducibility, signed distribution, restore correctness, cross-platform packaging or commercial readiness. [R18]

Existing test themes include compatibility parsing, filters/delete modes, symlinks, dossier signals, resume records/export/pruning, merge planning/materialisation, virtual archive-name transformations, fingerprint cache reuse/migration, binary descriptors, malformed text handling, reporting and staging. These are regression foundations. The acceptance plan in [VALIDATION.md](VALIDATION.md) specifies the additional evidence needed for the proposed commercial promises.

## 4. Source register

### Repository evidence

All source links below are pinned to the inspected commit. Line references identify the implementation inspected; symbol names remain the useful anchors if later refactoring moves it.

- **R1 — Source inventory and metadata:** [immutable tree](https://github.com/SWORDIntel/NIGHTINDEX/tree/d3e8e50684948bd81e01615acc74fd7448519743); [repository metadata endpoint](https://api.github.com/repos/SWORDIntel/NIGHTINDEX), observed 24 November 2026. Visibility metadata is time-sensitive.
- **R2 — Package, binaries and dependencies:** [Cargo.toml](https://github.com/SWORDIntel/NIGHTINDEX/blob/d3e8e50684948bd81e01615acc74fd7448519743/Cargo.toml).
- **R3 — `SCHEMA`:** [main.rs, lines 28–158](https://github.com/SWORDIntel/NIGHTINDEX/blob/d3e8e50684948bd81e01615acc74fd7448519743/src/main.rs#L28-L158).
- **R4 — Manifest writes and reads:** [`scan_command`, lines 1852–1890](https://github.com/SWORDIntel/NIGHTINDEX/blob/d3e8e50684948bd81e01615acc74fd7448519743/src/main.rs#L1852-L1890); [`load_label`, lines 9112–9130](https://github.com/SWORDIntel/NIGHTINDEX/blob/d3e8e50684948bd81e01615acc74fd7448519743/src/main.rs#L9112-L9130).
- **R5 — Scan errors and completion:** [main.rs, lines 1640–1730](https://github.com/SWORDIntel/NIGHTINDEX/blob/d3e8e50684948bd81e01615acc74fd7448519743/src/main.rs#L1640-L1730); [lines 1979–2004](https://github.com/SWORDIntel/NIGHTINDEX/blob/d3e8e50684948bd81e01615acc74fd7448519743/src/main.rs#L1979-L2004).
- **R6 — Hash provenance:** [reuse decisions, lines 1731–1792](https://github.com/SWORDIntel/NIGHTINDEX/blob/d3e8e50684948bd81e01615acc74fd7448519743/src/main.rs#L1731-L1792); [`blake3_file`, lines 9628–9642](https://github.com/SWORDIntel/NIGHTINDEX/blob/d3e8e50684948bd81e01615acc74fd7448519743/src/main.rs#L9628-L9642).
- **R7 — Content reads for fingerprints:** [profile construction, lines 1822–1851](https://github.com/SWORDIntel/NIGHTINDEX/blob/d3e8e50684948bd81e01615acc74fd7448519743/src/main.rs#L1822-L1851); [sampling/text readers, lines 6206–6303](https://github.com/SWORDIntel/NIGHTINDEX/blob/d3e8e50684948bd81e01615acc74fd7448519743/src/main.rs#L6206-L6303).
- **R8 — `dossier_confidence_tier`:** [main.rs, lines 1464–1490](https://github.com/SWORDIntel/NIGHTINDEX/blob/d3e8e50684948bd81e01615acc74fd7448519743/src/main.rs#L1464-L1490).
- **R9 — Virtual archive evidence:** [record construction/persistence, lines 2898–2998](https://github.com/SWORDIntel/NIGHTINDEX/blob/d3e8e50684948bd81e01615acc74fd7448519743/src/main.rs#L2898-L2998); [path construction, lines 3068–3098](https://github.com/SWORDIntel/NIGHTINDEX/blob/d3e8e50684948bd81e01615acc74fd7448519743/src/main.rs#L3068-L3098).
- **R10 — Binary comparison:** [`diff_files`, lines 47–105](https://github.com/SWORDIntel/NIGHTINDEX/blob/d3e8e50684948bd81e01615acc74fd7448519743/src/binary_diff.rs#L47-L105); [digest, lines 189–227](https://github.com/SWORDIntel/NIGHTINDEX/blob/d3e8e50684948bd81e01615acc74fd7448519743/src/binary_diff.rs#L189-L227); [FNV, lines 349–373](https://github.com/SWORDIntel/NIGHTINDEX/blob/d3e8e50684948bd81e01615acc74fd7448519743/src/binary_diff.rs#L349-L373).
- **R11 — Resume and execution:** [`ResumeRecorder`, lines 4769–4864](https://github.com/SWORDIntel/NIGHTINDEX/blob/d3e8e50684948bd81e01615acc74fd7448519743/src/main.rs#L4769-L4864); [resume plan, lines 5127–5181](https://github.com/SWORDIntel/NIGHTINDEX/blob/d3e8e50684948bd81e01615acc74fd7448519743/src/main.rs#L5127-L5181); [completion, lines 8570–8605](https://github.com/SWORDIntel/NIGHTINDEX/blob/d3e8e50684948bd81e01615acc74fd7448519743/src/main.rs#L8570-L8605).
- **R12 — Copy decisions and finalisation:** [skip decisions, lines 8417–8436](https://github.com/SWORDIntel/NIGHTINDEX/blob/d3e8e50684948bd81e01615acc74fd7448519743/src/main.rs#L8417-L8436); [regular copy, lines 8723–8742](https://github.com/SWORDIntel/NIGHTINDEX/blob/d3e8e50684948bd81e01615acc74fd7448519743/src/main.rs#L8723-L8742); [`CopyStage::finalize`, lines 308–334](https://github.com/SWORDIntel/NIGHTINDEX/blob/d3e8e50684948bd81e01615acc74fd7448519743/src/copy_exec.rs#L308-L334).
- **R13 — `copy_directory_tree`:** [main.rs, lines 4500–4551](https://github.com/SWORDIntel/NIGHTINDEX/blob/d3e8e50684948bd81e01615acc74fd7448519743/src/main.rs#L4500-L4551).
- **R14 — Preservation limits:** [README, lines 463–493](https://github.com/SWORDIntel/NIGHTINDEX/blob/d3e8e50684948bd81e01615acc74fd7448519743/README.md#L463-L493); [parser, lines 7614–7637](https://github.com/SWORDIntel/NIGHTINDEX/blob/d3e8e50684948bd81e01615acc74fd7448519743/src/main.rs#L7614-L7637); [symlink creation, lines 8744–8756](https://github.com/SWORDIntel/NIGHTINDEX/blob/d3e8e50684948bd81e01615acc74fd7448519743/src/main.rs#L8744-L8756).
- **R15 — Analysis-report persistence:** [main.rs, lines 3988–4013](https://github.com/SWORDIntel/NIGHTINDEX/blob/d3e8e50684948bd81e01615acc74fd7448519743/src/main.rs#L3988-L4013).
- **R16 — Names and scale:** [path conversion, lines 9607–9612](https://github.com/SWORDIntel/NIGHTINDEX/blob/d3e8e50684948bd81e01615acc74fd7448519743/src/main.rs#L9607-L9612); [copy-plan materialisation, lines 6689–6753](https://github.com/SWORDIntel/NIGHTINDEX/blob/d3e8e50684948bd81e01615acc74fd7448519743/src/main.rs#L6689-L6753); [archive inputs, lines 59–77](https://github.com/SWORDIntel/NIGHTINDEX/blob/d3e8e50684948bd81e01615acc74fd7448519743/src/archive_compare.rs#L59-L77).
- **R17 — Historical validation claim:** [PLAN.md](https://github.com/SWORDIntel/NIGHTINDEX/blob/d3e8e50684948bd81e01615acc74fd7448519743/PLAN.md); [current test modules](https://github.com/SWORDIntel/NIGHTINDEX/tree/d3e8e50684948bd81e01615acc74fd7448519743/src).
- **R18 — Manual release gate:** [scripts/release_check.sh](https://github.com/SWORDIntel/NIGHTINDEX/blob/d3e8e50684948bd81e01615acc74fd7448519743/scripts/release_check.sh).

### External technical references

These first-party references substantiate selected integration boundaries, not implemented NIGHTINDEX support. Accessed 24 November 2026; adapters must be pinned to their actual supported versions before release.

| Reference | Fact relevant to planning | Consequence proposed for NIGHTINDEX |
|---|---|---|
| [SQLite Online Backup API](https://sqlite.org/backup.html) | SQLite documents a coordinated API for backing up a live database. | Test a consistent backup/restore path instead of relying on an arbitrary copy of an active database file. |
| [The Sleuth Kit `fls`](https://sleuthkit.org/sleuthkit/man/fls.html) | `fls` can list image directory entries, including some deleted names; recursive listing does not follow deleted directories. | Treat imported lists as bounded evidence with tool/version and scope, not a complete lost-file inventory. |
| [OpenZFS v2.3 `zdb`](https://openzfs.github.io/openzfs-docs/man/v2.3/8/zdb.8.html) | The utility exposes on-disk debugging information and explicitly describes its output as unstable. | Retain raw output, version parsers and reject unsupported formats rather than silently inventing topology or filenames. |

Market/product sources and additional recovery references are cited alongside their claims in the [dossier](../../COMMERCIALISATION.md) and [product specification](PRODUCT_SPEC.md). Their presence is evidence that a capability exists elsewhere or has documented limits; it does not prove demand, uniqueness, technical parity or a price NIGHTINDEX customers will pay.
