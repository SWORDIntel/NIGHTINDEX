# Commercial acceptance and release validation

Prepared: **24 November 2026**. **Everything in the acceptance matrices below is proposed and unexecuted.** This document defines evidence required before the corresponding product claim can be made.

[Commercialisation dossier](../../COMMERCIALISATION.md) · [Roadmap](../../ROADMAP.md) · [Product specification](PRODUCT_SPEC.md) · [Feature catalogue](FEATURES.md) · [Repository baseline](BASELINE_AND_SOURCES.md)

## 1. Release principle and current evidence

The product must distinguish what existed, what was examined, what can be read, what was copied and what was verified. A successful command, a plausible candidate, or a copied image is insufficient evidence for all five. Every release claim should point to a supported configuration, a fixture with known truth, an executed result and any residual limits.

The inspected revision contains **103 static test attributes**. `PLAN.md` records an earlier claim of 102 passing tests; no tests/builds were rerun for this dossier because Cargo was unavailable. Existing test coverage and the manual gate are described in [BASELINE_AND_SOURCES.md](BASELINE_AND_SOURCES.md). These proposed acceptance cases add requirements; listing them does not indicate implementation or passing status.

Use generated fixtures with retained seeds, original inventories and expected digests, plus consented real cases when needed. Store fixture identity, tool versions, configuration, logs, outcomes and discrepancies. Preserve the original fixture; introduce faults only into disposable copies. Capture failures as well as successes. An unavailable configuration is **untested**, not passed.

## 2. Historical evidence, scan coverage and migration

| ID | Fixture or operation | Required result |
|---|---|---|
| V01 | Inventory consistent snapshots of the same declared scope before and after removing a known file. | Historical observation persists. The later snapshot supports absence within that source/scope/snapshot, without asserting universal loss. Other registered sources remain visible. |
| V02 | Interrupt scanning at multiple checkpoints; inject permission-denied, missing-directory and disconnect errors. Also move a file from an unvisited into an already visited directory during an otherwise completed live scan. | Failures produce explicit partial/failed/cancelled state. Previous history survives and unexamined entries never become confirmed absence. A completed live traversal means “not observed”, not a point-in-time absence claim. Resume exposes coverage without silent skipped work. |
| V03 | Change exclusions, scan root, access rights or mount scope between generations. | Coverage differences are recorded. Excluded/unreachable subtrees remain unexamined; the UI explains why comparisons cannot establish disappearance there. |
| V04 | Import a legacy NIGHTINDEX database containing stale path rows and reused hashes. | Preserve original artifact/hash and available timestamps. Mark unknown scan completeness, verification times and overwritten history as unknown. Never fabricate historical generations that the source did not retain. |
| V05 | Modify a file while preserving size and mtime; rescan with cache reuse, then force fresh verification. Separately mutate content during hashing. | Cached evidence retains its original full-read time and cache provenance. Fresh reads of a stable source detect the changed bytes. Concurrent mutation is detected under the declared consistency contract or stability remains unproven; a digest of a read stream alone cannot certify a stable file version. |
| V06 | Same name/size, renamed copies, an older version, partial fingerprints and full-digest matches. | Candidate relationships remain candidates until their required verification occurs. Exact matching retains separate occurrences; an older version does not resolve a lead for a newer one. |
| V07 | Later discover an exact copy of a file from a previously closed case. | Append a resolution/reopening event with provenance. Keep the old loss assessment, search history and source failure; do not rewrite them as if the copy had always been known. |
| V08 | Clock skew, coarse timestamps, unknown timezone and an imported file modification date. | Keep event/observation/import/file times distinct with precision and provenance. Do not report modification time as last verified time. |

Additional acceptance assertions: “no catalogue record” never means “never existed”; “no verified copy among examined sources” always identifies its saved scope; a user assessment of permanent loss remains attributed and reversible. These are product semantics and should be exercised through both CLI output and GUI presentation.

## 3. Media, acquisition and recovery evidence

| ID | Fixture or operation | Required result |
|---|---|---|
| V09 | Reuse the same mountpoint or device name for a different disk during scan/resume. | The operation cannot silently bind to the replacement. Identity mismatch becomes an actionable blocked state; no source history moves merely because a path matches. |
| V10 | Clone a filesystem with identical UUIDs; reconnect through an adapter with missing/changed identifiers. | Original and clone remain separate media assets with lineage. Ambiguity is preserved and resolved explicitly, rather than relying on one purportedly unique attribute. |
| V11 | Analyse an image with a known unread-region map; include valid sparse-zero regions separately. | Acquisition holes remain distinct from genuine logical zeros. A digest of the image or a copied file is not presented as proof of intact original bytes when affected regions are unknown/unread. |
| V12 | Supply a mismatched, truncated or absent mapfile; change image bounds or sector assumptions. | Reject incompatible associations or record unknown acquisition coverage. Never infer full acquisition from image size or process success. Record the evidence used to associate image and map. |
| V13 | Image metadata permits reliable extent mapping in one fixture and no reliable mapping in another. | Flag affected files only where the mapping supports it. Otherwise state unknown impact; do not present acquisition percentage as recovered-file percentage. |
| V14 | Supported complete/degraded array images, missing members, damaged labels, unread metadata and unavailable encryption keys. | Report observed topology, actual recovered metadata and inaccessible dependencies separately. Labels alone do not become filenames. No invented paths or content; locked data remains locked. |
| V15 | Change an external parser/tool version or feed malformed diagnostic output. | Retain raw output and version. Unsupported formats fail visibly or remain unparsed; they never produce plausible but ungrounded media/file records. |

Topology discovery, metadata salvage and content recovery each require their own supported-layout fixture set. A passing complete-pool case does not qualify a missing-member case. `zdb` output is explicitly unstable according to [OpenZFS documentation](https://openzfs.github.io/openzfs-docs/man/v2.3/8/zdb.8.html); parser-version coverage is a real release dependency. No physical repair, chip-off or arbitrary RAID reconstruction claim follows from these tests.

## 4. Imports, archive contents and names

| ID | Fixture or operation | Required result |
|---|---|---|
| V16 | Import the same manifest twice, resume a partial import, then import a revised artifact. | Idempotence is defined and tested. Preserve artifact identity and revisions without silently multiplying occurrences or collapsing distinct sources. Skipped/malformed rows have explicit reasons. |
| V17 | Hostile absolute/parent-traversal paths, symlink targets, shell syntax, spreadsheet formulas and markup in imported metadata. | Import, preview and export cannot execute content or write outside the selected destination. Keep raw evidence with safe display/export forms. Text is not treated as instructions. |
| V18 | Real archives with duplicate names, nested containers, encryption, truncation and unsupported formats. | Record enumerated members and their container/version provenance. Distinguish unlisted/encrypted/failed portions from absent members. Archive-name inference is never shown as parser-confirmed contents. |
| V19 | Archive/decompression workload reaches depth, byte, member-count, memory or time limits. | Stop predictably, preserve partial coverage, identify the limit and keep the interface responsive. Cancellation releases workers/resources. No automatic unbounded extraction is required to search known metadata. |
| V20 | Raw/non-UTF-8 names, case collisions, Unicode variants, long paths and platform-reserved names. | Preserve a lossless original representation where supported. Preflight target collisions and unsupported names; any mapping is explicit, reversible and recorded. Display conversion cannot collapse identity. |

Imported recovery listings prove only what the source format and extraction scope support. For example, the [Sleuth Kit `fls` manual](https://sleuthkit.org/sleuthkit/man/fls.html) describes limits on following deleted directories. Test missing parents and orphan entries; never invent a full original path from a basename alone.

## 5. Reconciliation, copying and preservation

| ID | Fixture or operation | Required result |
|---|---|---|
| V21 | Source changes between planning, opening, reading and finalising a copy. | Detect changes within the declared consistency contract, or report unverified/unstable output. A plan binds to source identity/version evidence; resume cannot silently substitute a later manifest row. |
| V22 | Interrupt at staging, write, flush, rename, verification and journal checkpoints; restart repeatedly. | Completed durable actions and pending work reconcile correctly. No false verified state, silent truncation or duplicated destructive action. Recovery of temporary files follows a documented policy. |
| V23 | Concurrent writer creates/replaces destination; destination is a symlink; no-clobber policy is selected. | No unintended replacement or write escape. Test actual supported filesystem primitives; a preflight existence check alone is insufficient. Conflicts remain reviewable. |
| V24 | Full destination, quota failure, disconnect, read error and unsupported metadata during copy. | Failures identify affected entries. Job totals distinguish completed, copied-unverified, verified, skipped and failed work. Retry is bounded and resumable; a partial job is not presented as complete. |
| V25 | Preserved timestamps, permissions, ACLs, xattrs, hardlinks, sparse layout and available platform-specific streams. | Verify each promised attribute on each supported source/target pair. Report unsupported or intentionally transformed attributes before execution and in the result. Content equality alone does not establish metadata preservation. |
| V26 | Selective copy, merge, keep-both, overwrite/backup and any delete-capable workflow. | Apply the same identity, error, completeness and verification contract across paths. Dry-run predicts intended actions; later environmental changes trigger revalidation. Destructive decisions are never inferred solely from similarity. |

Distinguish verification of a copy against the bytes read from its source from verification against an independent historical digest. Neither replaces validation of an incomplete source acquisition. Define separately what a successful read-back, durability boundary and preservation result mean on each platform.

## 6. Catalogue survival, privacy and GUI

| ID | Fixture or operation | Required result |
|---|---|---|
| V27 | Back up a catalogue under writes; restore on a clean machine without original disks; fail backup midway. | Use a database-supported consistent method, restore relationships/history/artifact references, and verify the backup. Interrupted output is not advertised as a valid backup. The location is independent of the media being catalogued. |
| V28 | Upgrade/downgrade attempts, interrupted migration, corrupted catalogue/export and licence expiry. | Preserve an independently restorable copy before migration; reject unsupported schema operations clearly. Historical search/export access follows the product's continuity promise, including licence expiry. Old verification events remain intact while policy can mark a recheck due. |
| V29 | Export a customer report/support bundle with sensitive filenames, identifiers, paths and notes. | Redaction is previewable and consistent; raw evidence is retained separately under the chosen policy. No automatic customer-content upload. Support diagnostics disclose their contents before sharing. |
| V30 | Keyboard-only navigation, accessibility inspection, long searches, worker crash, cancellation and reconnect. | Core tasks remain usable; tables do not block the UI, status remains understandable, and evidence is reachable. Background failures are visible. Restart preserves decisions/checkpoints. |

SQLite provides a coordinated [Online Backup API](https://sqlite.org/backup.html). The implementation may choose an appropriate supported mechanism, but a successful restore drill—not merely copying a file—qualifies catalogue durability.

The core usability exercise is concrete: given a catalogue and labelled media, an operator locates a historical file, sees the remaining search gaps, reconnects the correct source, verifies a candidate and exports the finding. Record completion time, mistakes, help needed and false interpretations. A visually attractive screen does not by itself pass this exercise.

## 7. Performance and supported configurations

Set performance thresholds **before** qualification runs, using measured baseline results and pilot requirements. This dossier supplies no invented throughput, scale, recovery rate or latency achievement. Record file/byte counts, depth, path lengths, duplicates, archive composition, device/interface, filesystem/driver, CPU/RAM, catalogue size, cache state, read profile and tool versions.

Measure time to first useful result, total scan/import duration, peak memory, catalogue growth, cold/warm search latency, GUI response under work, cancellation/checkpoint latency and verification cost. Test at the declared support limit and beyond it: overload should become a visible limit or degraded operation, not silent omissions. Cap pathological candidate comparisons and parser workloads. Publish the tested envelope with the release.

Initial qualification should stay narrow:

| Configuration class | Proposed qualification scope |
|---|---|
| Primary operator environment | Pin a Debian/Linux version and desktop packaging target; test healthy mounted-file inventory and catalogue operations on selected filesystems. This is a proposed first target, not current certification. |
| Filesystem copy pairs | Name each source/target filesystem and driver version; qualify bytes, naming and preservation individually. Reading a mounted ZFS dataset does not qualify raw ZFS recovery. |
| Offline catalogue use | Test search, history and export without connected sources, internet access or optional recovery tools. |
| Images and recovery adapters | Publish supported image formats, acquisition-map associations, tool versions and exact array/layout cases independently. |
| Other operating systems | Add native builds, filesystem semantics, packaging and accessibility qualification before advertising support. |

## 8. Manual release gate and evidence record

Retain the project's manual release workflow. On the qualified build host, run the existing gate from the repository root:

```bash
bash scripts/release_check.sh
```

Resolve the documented Clippy debt and qualify strict mode before making it a required commercial gate:

```bash
NIGHTINDEX_STRICT_CLIPPY=1 bash scripts/release_check.sh
```

For a release candidate, record commit, dependency lockfile, compiler/OS/tool versions, command outcomes, applicable acceptance cases, fixture identities, measured limits, packaging install/uninstall results, catalogue upgrade/restore results and unresolved exclusions. Qualify shipped binaries and update procedures, not only a developer build. Supply checksums/signatures and dependency/licence notices according to the chosen distribution model; verify their generation and checking.

A release decision requires the cases applicable to its advertised features to pass, with failures either fixed or the affected capability excluded from its support statement. Unsupported recovery cases remain clearly unsupported. Do not convert unfinished acceptance rows into checkmarks because the ordinary unit suite passes. This plan adds no CI workflow, test implementation or runtime feature by itself.
