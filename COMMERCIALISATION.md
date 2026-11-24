# NIGHTINDEX commercialisation dossier

**Planning date:** 24 November 2026

**Status:** Proposed product and commercial strategy; implementation and customer validation remain separate work.

**Reviewed source:** [`d3e8e50684948bd81e01615acc74fd7448519743`](https://github.com/SWORDIntel/NIGHTINDEX/tree/d3e8e50684948bd81e01615acc74fd7448519743)

This dossier defines the product outcome, prospective customers, commercial experiments and conditions for a defensible release. The [roadmap](ROADMAP.md) sequences delivery. The [product specification](docs/commercialisation/PRODUCT_SPEC.md), [feature register](docs/commercialisation/FEATURES.md), [baseline and sources](docs/commercialisation/BASELINE_AND_SOURCES.md), and [validation plan](docs/commercialisation/VALIDATION.md) provide the detailed requirements and evidence.

## 1. Executive recommendation

Develop NIGHTINDEX into a local application for **understanding, reconciling and remembering a collection of files across physical media and time**. Its central promise should be an evidence-backed answer to four questions:

1. What files do the available records establish that this collection contained?
2. Where were they last observed, and which copies can be verified now?
3. Which records remain unresolved, and what sources have not been examined?
4. What action would most usefully reduce that uncertainty?

The initial commercial opportunity to test is a managed reconciliation service, followed by a desktop product for repeat users. A customer supplies a collection of hard drives, readable images, backup inventories or previous recovery outputs; the service returns an organised account of the collection and its exceptions. It should charge for a defined process and deliverable, with recovery work separately scoped where appropriate.

The most distinctive product hypothesis is durable **loss accounting**. A lost file can impose repeated search costs long after the original failure. A catalogue that preserves evidence of its former existence, records what has been searched, and remembers the remaining leads can prevent those searches being restarted from scratch. This value exists even when no additional content can be recovered.

A strong graphical interface is a release requirement for the desktop product. The underlying priorities are stable media identity, persistent observation history, explicit scan coverage, reliable reconciliation and verification. An attractive interface cannot compensate for a catalogue that mistakes stale entries for present files or interprets an interrupted scan as deletion.

The commercial proposition remains a hypothesis. Existing products already catalogue offline disks, find duplicates, search backups and recover damaged files. NIGHTINDEX must demonstrate that its combined workflow saves enough time and resolves enough uncertainty for a particular customer to pay. No uniqueness, market-size or revenue claim is made here.

## 2. The customer problem and the result to deliver

The starting situation is a batch of unknown or poorly documented media: external drives, older backups, renamed directories, partial migrations and overlapping recovery attempts. The operator may know that a project existed but cannot identify the relevant disk, distinguish versions, or determine whether a surviving copy was actually checked.

The useful result is more than a searchable list. It is a record connecting a file or version to its observations, content evidence, physical locations and unresolved questions. Search results should explain the evidence in ordinary language. A synthetic example:

> `project-atlas/final-model.bin` was recorded on medium `ARCHIVE-017` during the 2024 inventory. Its recorded full-file digest matches a copy verified on `WORK-004` today. Another same-named file has different content. `OFFSITE-003` has not been examined in this case.

Another valid result is:

> This filename is present in an imported 2022 catalogue. No matching content has been verified among the examined sources. Two media remain unexamined; one directory scan was incomplete. The record remains unresolved.

The product must not turn that second result into “permanently lost.” A prior observation can establish historical existence; surviving filesystem metadata may provide additional clues; absence from the examined sources establishes only that scoped absence. No record is not evidence that a file never existed.

When a newly connected disk resolves an outstanding record, NIGHTINDEX should connect the new evidence to the earlier history and retain how the conclusion changed. Users should not have to maintain a separate spreadsheet of forgotten searches.

## 3. Current foundation and material gaps

The reviewed source provides a Rust/SQLite command-line foundation: directory manifests, optional full-file BLAKE3 hashing, heuristic matching, comparisons, copy plans, resume records and analysis-report persistence. The two command names use the same source entry point. These observations come from the [package configuration](https://github.com/SWORDIntel/NIGHTINDEX/blob/d3e8e50684948bd81e01615acc74fd7448519743/Cargo.toml) and [source snapshot](https://github.com/SWORDIntel/NIGHTINDEX/tree/d3e8e50684948bd81e01615acc74fd7448519743/src); they do not certify runtime behaviour.

Several gaps materially affect the proposed product:

- Manifest rows are keyed by label and path. Rescans overwrite observed rows while old ordinary-file rows can remain; there is no durable scan-generation and coverage model separating present observations from historical clues. Existing report history is not an immutable file-history ledger. See the [schema](https://github.com/SWORDIntel/NIGHTINDEX/blob/d3e8e50684948bd81e01615acc74fd7448519743/src/main.rs#L28-L158) and [scan updates](https://github.com/SWORDIntel/NIGHTINDEX/blob/d3e8e50684948bd81e01615acc74fd7448519743/src/main.rs#L1852-L1890).
- Current virtual archive rows are inferred from filenames, paths and suffixes. They do not enumerate actual archive interiors. Parser-backed archive cataloguing is new work. See [virtual member construction](https://github.com/SWORDIntel/NIGHTINDEX/blob/d3e8e50684948bd81e01615acc74fd7448519743/src/main.rs#L2898-L2998).
- Staged copying and resume records are useful foundations, but the inspected regular-copy path does not perform subsequent full-content read-back verification. A completed copy must not be sold as a verified recovery. See the [copy path](https://github.com/SWORDIntel/NIGHTINDEX/blob/d3e8e50684948bd81e01615acc74fd7448519743/src/main.rs#L8723-L8742).
- No desktop GUI, physical-media catalogue, block imaging or RAID/ZFS reconstruction implementation was found in the reviewed source. The preservation contract also needs work: several compatibility flags are explicitly documented as no-ops in the [baseline README](https://github.com/SWORDIntel/NIGHTINDEX/blob/d3e8e50684948bd81e01615acc74fd7448519743/README.md#L463-L493).

The baseline review is source inspection, not an executed build, benchmark or recovery trial. The roadmap therefore begins with truthful capability labels and evidence semantics before expanding commercial claims.

## 4. Prospective buyers and jobs

These segments are interview hypotheses. Interest, urgency and available budget must be established separately.

| Prospective buyer | Trigger | Result they might pay for | Uncertainty to test |
|---|---|---|---|
| Small technical or creative organisation | Accumulated drives, staff handover or an undocumented migration | A searchable collection with authoritative-copy decisions, exception report and media locations | Does the problem recur, and who authorises spending? |
| MSP or independent systems operator | A client supplies mixed storage and conflicting backups | Repeatable intake, review and handover that reduces operator hours | Does NIGHTINDEX fit existing tools and work without founder assistance? |
| Researcher or technical archivist | Important older data dispersed across offline sources | Historical existence search, persistent unresolved records and a preservation plan | Is willingness to pay sufficient for the support required? |
| Recovery provider, later | Multiple recovered datasets need matching against expectations | Imported evidence, content reconciliation and understandable completeness reports | Which formats, service rights and specialist workflows are required? |

Select two cohorts for discovery rather than building for every category simultaneously. Professional investigation, large shared deployments and formal evidential use remain possible extensions, but their requirements must be established before making corresponding claims.

## 5. Product capabilities that support the purchase

### 5.1 Persistent history and loss accounting

Preserve file observations across scans, renamed paths, disconnected media and imported catalogues. Store the source, observation time, relevant version, digest provenance and coverage context. A failed rescan must not erase historical knowledge. Search should cover online files and offline records together while making their different states clear.

Offer a dedicated unresolved-file view with saved searches, operator notes, examined sources and next leads. Distinguish exact verified content, historical content evidence, candidate matches, metadata-only records and unknown areas. Similarity can prioritise review; it must not silently establish identity. Reports need to retain both their original evidence and subsequent resolutions.

### 5.2 Physical media and collection awareness

Represent physical devices, partitions, logical volumes, images, pools and individual file occurrences as related objects. A human label, shelf/location and optional printed identifier help the user find the actual medium. Hardware and filesystem identifiers are evidence, with explicit handling for missing identifiers, cloned volumes and changing USB enclosures.

Useful adjacent capabilities include media photographs, check-in/check-out, per-medium notes, reconnect prompts and a queue of sources worth examining next. A “single verified copy” warning must mean one among known and examined copies, with verification dates and shared failure domains visible. Two paths on one device do not automatically provide independent protection.

### 5.3 Reconciliation and controlled execution

Show exact duplicates, likely renamed trees, conflicting versions and incomplete project groups. Preserve explanations for match ranking. Let the operator review a durable plan before copying, merging or retiring data. Bind execution to the intended sources and destinations and detect changed conditions before resuming.

A commercial copy mode needs an explicit preservation and verification contract: which timestamps, permissions, links, extended attributes, sparse regions and platform-specific metadata are supported; which failures stop the job; and how fresh verification is recorded. Unsupported preservation requests must be visible. Deletion and destructive deduplication should have separate review and retention controls.

### 5.4 A desktop GUI designed around cases

The main workflow should be **intake → inventory → search/history → compare → review plan → execute → verify → hand over**. Keep progress, read budgets, warnings, cancellation and resume accessible without burying the operator in implementation details. Provide searchable tables, filters, comparison explanations, keyboard operation, accessible status indicators and useful empty/error states.

Retain a supported CLI and a reusable engine interface so automation and the GUI share semantics. Choose the GUI framework through a bounded prototype addressing packaging, responsiveness, accessibility and licence requirements. The choice should follow these results, not require a database rewrite or a hosted account system.

### 5.5 Recovery evidence and realistic salvage

Import previous manifests, backup inventories, filesystem metadata and recovery-tool lists with their provenance. Add actual archive readers with declared formats, encrypted/truncated states, nesting limits and resource budgets. Preserve the distinction between seeing an archive member name and successfully reading its bytes.

Physical-media awareness should later include array topology and the relationship between supplied members and missing components. Partial RAID/ZFS work may yield useful metadata, names or some content when the relevant structures and blocks remain readable. Neither filenames nor recoverable bytes can be guaranteed from an arbitrary surviving member. Implement narrowly defined, fixture-tested salvage capabilities and report precisely what was observed.

For failing media, prefer controlled integration with established imaging tools and retained images/mapfiles. Recovery remains a separate stage from accounting for the resulting files. The [feature register](docs/commercialisation/FEATURES.md) covers additional opportunities such as project completeness, catalogue imports, integrity review, optional previews, portable case bundles and team workflows.

## 6. Existing alternatives and positioning

The following comparison records first-party documented capabilities reviewed on 24 November 2026. It is not a hands-on benchmark or a complete list of each product's features. An omitted capability must not be interpreted as absent.

| Product/category | Documented capabilities relevant here | Implication for NIGHTINDEX |
|---|---|---|
| [NeoFinder](https://www.cdfinder.de/guide/1/info.html) | Offline browsing/search, rich metadata, some archive contents, batch cataloguing and network sharing | Offline search and metadata are baseline expectations. Test the additional value of evidence history and reconciliation. |
| [WinCatalog](https://www.wincatalog.com/features.html) | Catalogue exports, duplicate matching, media locations/loans, copying from results and read-only operation after trial expiry | Physical organisation, portability and continued access are established product patterns. |
| [Virtual Volumes View](https://www.fsoft.it/VVV/) | Offline-volume searching, a unified virtual filesystem and catalogue sharing across host platforms | A collection view alone is insufficient differentiation. |
| [rmlint](https://rmlint.readthedocs.io/en/master/rmlint.1.html) | Hash or byte-comparison duplicate detection, machine-readable output and separation of detection from generated removal actions | Reuse useful interchange patterns; keep matching and destructive policy distinct. |
| [rclone bisync](https://rclone.org/bisync/) | Bidirectional synchronisation, configurable comparisons, conflict handling and access checks | Missing mounts and incomplete views are concrete hazards; explicit coverage belongs in the core model. |
| [restic](https://restic.readthedocs.io/en/stable/045_working_with_repos.html) | Snapshot inventories and file search across snapshots, including machine-readable output | Import backup evidence while retaining snapshot identity and verification status. |
| [UFS Explorer RAID Recovery](https://www.ufsexplorer.com/ufs-explorer-raid-recovery/) | Virtual RAID assembly, RAID-Z support and use of bad-sector maps | Specialist recovery is substantial existing capability; integrate first and scope native additions. |
| [R-Studio](https://www.r-studio.com/Unformat_Help/r-studiofeat.html) | Filesystem/RAID recovery and recovery-list export/re-import | Recovery lists are promising evidence inputs; confirm formats and permitted commercial use. |
| [GNU ddrescue](https://www.gnu.org/software/ddrescue/ddrescue) | Read-error-aware imaging and resumable mapfiles | An imaging adapter can retain established acquisition evidence without implementing a new imaging engine. |

The positioning to test is a coherent, local workflow joining these concerns: historical memory, current-copy evidence, media identity, reconciliation and accountable handover. No competitor's lack of that exact workflow has been established by this review. Pilot comparisons should use the customer's actual existing process.

## 7. First paid offer: a bounded reconciliation case

Begin with readable media, existing images and exported inventories within a declared support envelope. Scope damaged-hardware acquisition separately. Agree what the customer wants resolved, what sources will be examined, what actions are permitted and what report they will receive.

The proposed package contains:

1. **Intake and media map:** identifiers, supplied labels, sources, condition observations and relationships.
2. **Catalogue and coverage report:** discovered records, scan dates, excluded paths, access failures and unexamined media.
3. **Reconciliation findings:** verified matches, candidates, conflicts, likely project relationships and unresolved records.
4. **Optional execution:** reviewed copies or migrations under a stated preservation and verification policy.
5. **Portable handover:** readable report, machine-readable export, recorded decisions, verification evidence and recommended next sources/actions.

Deliverables should remain usable without the original operator or an active subscription. State what the case established and what it could not establish. Do not sell a guarantee that all files have been found or that failed media can be recovered.

Use the service to discover repeatable work, support burden and missing interface affordances. Keep operator interventions visible; a result delivered through substantial manual repair is not evidence that the software can already automate the case.

## 8. Customer validation and acceptance

Run **6–10 discovery interviews** across the two selected cohorts. Ask about the last real event: what was supplied, what was searched, what tools were bought, where time was spent, who approved spending and what evidence ended the uncertainty. A favourable reaction to a demo is weaker evidence than a recent, budgeted problem.

Then seek **3–5 independently paid pilot cases** with bounded inputs and written deliverables. These are proposed learning targets, not a statistical claim of product-market fit. Maintain a discovery log distinguishing reported pain, observed behaviour, quotations, payments and repeat demand.

Measure:

- Time to a useful inventory and to an answer for representative historical-file queries.
- Operator minutes per medium and exception, separately from unattended machine time.
- Resolution of known records by evidence category, retaining unexamined and unknown cases in the account.
- Visibility of every excluded source, failed subtree and incomplete operation in the report.
- False exact-match, unsupported-loss and false-verification assertions; the acceptance target is zero in the ground-truth corpus and pilots.
- Whether a second operator/customer can locate a named medium and explain a file's state without founder assistance.
- Quote acceptance, actual payment, repeat engagement and willingness to pay for a specified software offer.
- Direct labour, support time, external-tool costs, storage/compute and contribution per case.

The preliminary continuation gate is at least three independently paid bounded cases, reproducible delivery, demonstrable customer time saved and acceptable support economics. If a segment fails, narrow the use case or revise the offer before adding features. An unpaid pilot can establish usability but cannot validate a price.

## 9. Revenue and packaging hypotheses

**Managed service.** Test a fixed intake fee plus a scoped reconciliation quote. Media count/type, file count, verification volume, metadata condition and review complexity should influence scope. Separate imaging, hardware, urgency and ongoing storage. Per-TB-only pricing may poorly reflect the labour involved in many small files or ambiguous versions.

**Desktop operator product.** Test a local licence with a defined support/update entitlement. Compare perpetual-version plus maintenance/major upgrades against an annual subscription using actual offers. Preserve catalogue reading and export after expiry. Do not require cloud connectivity for historical-file answers.

**Technician/team product, later.** Test whether multiple cases, review sign-off, operator attribution, report templates, shared catalogues and service rights support an additional offer. Build multi-user or hosted operation only when a paying workflow justifies the operational burden.

Record the exact offer, quoted price, scope, acceptance/refusal reason, actual work and support. Compute case contribution as collected revenue less direct labour, payment fees, external licences, media, compute/storage and expected support/refund allowance. Count founder labour. Compare support demand with sustainable operator capacity before promising response times.

No launch price, revenue forecast or customer count is committed here. Pricing experiments should test materially different packages with comparable prospects and avoid treating discounts or personal favours as normal demand.

## 10. Distribution, support and ownership decisions

Start with a declared host/filesystem support matrix and controlled installers/packages. Provide checksums/signatures, versioned report formats, migration and catalogue-restore procedures, release notes and a clear update policy. Expand platforms only when their path, metadata and packaging behaviour have been validated.

The catalogue itself is sensitive: filenames, locations, previews and relationships can reveal information without the source content. Default to local processing and minimal collection. Make previews, content extraction, semantic enrichment, diagnostic uploads and telemetry separately understandable and controllable. Support exports need review/redaction rather than automatic disclosure of catalogue contents.

Before distribution, determine code ownership, contribution provenance, product terms and dependency obligations. The reviewed [repository tree](https://github.com/SWORDIntel/NIGHTINDEX/tree/d3e8e50684948bd81e01615acc74fd7448519743) contains no root licence file, and [Cargo metadata](https://github.com/SWORDIntel/NIGHTINDEX/blob/d3e8e50684948bd81e01615acc74fd7448519743/Cargo.toml) declares no licence value. This dossier grants no rights, changes no terms and makes no licensing decision on the owner's behalf.

Commercial binaries, selected public documentation and privately developed capabilities are possible packaging choices subject to the actual rights and dependency review. There is no requirement in this strategy to publish implementation details. A licence is a rights framework; it cannot physically prevent copying.

Review GUI/runtime and imaging/recovery dependencies before bundling. GNU ddrescue's project page specifies GPL licensing; a separate-process design does not by itself settle every distribution arrangement. Obtain the necessary permissions before redistributing commercial components or implying partnerships.

Define support boundaries, intake information, reproducible diagnostics, data retention and escalation. Avoid unsupported descriptions such as “forensic grade,” certified, guaranteed complete or universally recoverable. Claims should map to evidence in the [validation plan](docs/commercialisation/VALIDATION.md).

## 11. Risks, decisions and stop conditions

| Risk | Decision or response |
|---|---|
| Stale rows presented as current files; incomplete scans presented as loss | Complete observation/coverage semantics before selling loss-accounting claims. |
| Similarity presented as identity or copying presented as verification | Separate evidence types and enforce their meaning in UI, reports and automation. |
| Recovery ambitions absorb the product | Fund narrowly validated integrations and salvage from demonstrated customer needs. |
| GUI becomes a presentation layer over unreliable state | Prototype against the real job/status API and failure cases. |
| Attractive demos hide heavy founder labour | Require second-operator delivery and track intervention time. |
| Customer support consumes the margin | Narrow supported inputs, improve diagnostics and revise scope/pricing. |
| Customers prefer existing catalogue/backup tools | Identify a demonstrated reconciliation advantage or stop that segment. |
| Catalogue loss recreates the original uncertainty | Treat export, backup, restore and schema migration as release requirements. |
| Scope expands into every storage technology | Use phased support matrices and explicit deferred work. |

Stop or re-scope a pilot if source safety, evidence accuracy or agreed data handling cannot be maintained. Pause a commercial release when exact-match/loss assertions fail ground-truth checks, verification is ambiguous, catalogue restoration fails, or support commitments exceed capacity. These are product credibility requirements, not optional polish.

## 12. Immediate decisions and first actions

Approve the evidence semantics and capability labels in the companion documents; preserve the useful CLI foundation while implementing the smallest durable catalogue model. Define the first supported host/filesystem/transport combination and the exact first-pilot deliverable. Build a synthetic case showing an offline historical record, a changed version, a verified surviving copy and an unexamined source.

Recruit the discovery cohort, document actual workflows and prepare a bounded paid offer. In parallel, prototype the GUI's intake, historical search and unresolved-record views using the same case model. Establish ownership/licensing decisions before distributing a commercial package.

Advance through the [roadmap](ROADMAP.md) according to demonstrated exit criteria. The objective is a product that users can trust to remember their collection, explain what is known, and preserve the work already spent understanding what remains missing.
