use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_yaml;
use walkdir::WalkDir;

mod copy_exec;

use copy_exec::{CopyFinalizeOutcome, CopyStager};

use copy_exec::{
    CopyProgressSnapshot, format_progress_line, format_start_line, format_summary_line,
    write_copy_progress_event, write_copy_summary_event,
};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS files (
    label TEXT NOT NULL,
    rel_path TEXT NOT NULL,
    file_type TEXT NOT NULL,
    size INTEGER NOT NULL,
    mtime_ns INTEGER NOT NULL,
    fast_hash TEXT,
    scanned_at INTEGER NOT NULL,
    PRIMARY KEY (label, rel_path)
);

CREATE INDEX IF NOT EXISTS idx_files_label_path ON files(label, rel_path);

CREATE TABLE IF NOT EXISTS copy_runs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    left_label TEXT NOT NULL,
    right_label TEXT NOT NULL,
    mode TEXT NOT NULL,
    planned_files INTEGER NOT NULL,
    copied_files INTEGER NOT NULL,
    bytes_to_copy INTEGER NOT NULL,
    copied_bytes INTEGER NOT NULL,
    duration_ns INTEGER NOT NULL,
    copied_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_copy_runs_pair_mode_at
ON copy_runs(left_label, right_label, mode, copied_at);

CREATE TABLE IF NOT EXISTS copy_resume_sessions (
    session_id TEXT PRIMARY KEY,
    mode TEXT NOT NULL,
    left_label TEXT NOT NULL,
    right_label TEXT NOT NULL,
    source_root TEXT,
    destination_root TEXT,
    started_at INTEGER NOT NULL,
    finished_at INTEGER,
    planned_files INTEGER NOT NULL DEFAULT 0,
    copied_files INTEGER NOT NULL DEFAULT 0,
    failed_files INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS copy_resume_items (
    session_id TEXT NOT NULL,
    rel_path TEXT NOT NULL,
    status TEXT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    bytes_done INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (session_id, rel_path)
);

CREATE INDEX IF NOT EXISTS idx_copy_resume_items_status
ON copy_resume_items(session_id, status);

CREATE TABLE IF NOT EXISTS file_fingerprints (
    label TEXT NOT NULL,
    rel_path TEXT NOT NULL,
    normalized_name TEXT NOT NULL,
    normalized_folder TEXT NOT NULL,
    ext TEXT,
    is_binary INTEGER NOT NULL,
    is_archive INTEGER NOT NULL,
    archive_family TEXT,
    language TEXT NOT NULL DEFAULT 'unknown',
    size_class TEXT NOT NULL DEFAULT 'large',
    binary_signature TEXT,
    binary_descriptor TEXT,
    text_signature TEXT,
    archive_signature TEXT,
    scanned_at INTEGER NOT NULL,
    PRIMARY KEY (label, rel_path)
);

CREATE INDEX IF NOT EXISTS idx_file_fingerprints_label_rel_path
ON file_fingerprints(label, rel_path);

CREATE TABLE IF NOT EXISTS virtual_archive_members (
    label TEXT NOT NULL,
    rel_path TEXT NOT NULL,
    virtual_path TEXT NOT NULL,
    virtual_member TEXT NOT NULL,
    archive_family TEXT,
    payload_signature TEXT,
    archive_depth INTEGER NOT NULL,
    size INTEGER NOT NULL,
    mtime_ns INTEGER NOT NULL,
    fast_hash TEXT,
    scanned_at INTEGER NOT NULL,
    PRIMARY KEY (label, rel_path)
);

CREATE INDEX IF NOT EXISTS idx_virtual_archive_members_label_virtual_member
ON virtual_archive_members(label, virtual_member);

CREATE TABLE IF NOT EXISTS signature_cache (
    cache_key TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    rel_path TEXT NOT NULL,
    file_type TEXT NOT NULL,
    size INTEGER NOT NULL,
    mtime_ns INTEGER NOT NULL,
    fast_hash TEXT,
    value_json TEXT NOT NULL,
    computed_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_signature_cache_kind_path
ON signature_cache(kind, rel_path);
"#;

const DEFAULT_NOISE_DIRS: &[&str] = &[
    ".cache",
    ".DS_Store",
    ".git",
    ".nobackup",
    ".svn",
    ".Trash",
    ".Trashes",
    "$RECYCLE.BIN",
    "$RECYCLE",
    "Desktop.ini",
    "Node_modules",
    "Recovered Files",
    "System Volume Information",
    "Temporary Items",
    "Thumbs.db",
    "tmp",
    "temp",
];
const COMPARE_SUMMARY_REPORT_SCHEMA: &str = "nightindex.compare_summary";
const DOSSIER_REPORT_SCHEMA: &str = "nightindex.dossier";
const ARCHIVE_MEMBER_DIFF_REPORT_SCHEMA: &str = "nightindex.archive_member_diff";
const ARCHIVE_MEMBER_PLAN_REPORT_SCHEMA: &str = "nightindex.archive_member_plan";
const ARCHIVE_MEMBER_PLAN_ROW_SCHEMA: &str = "nightindex.archive_member_plan.row";
const REPORT_VERSION_V1: u32 = 1;

const ARCHIVE_EXTENSIONS: &[&str] = &[
    ".tar.gz", ".tar.xz", ".tar.bz2", ".zip+txt", ".img.raw", ".zip", ".7z", ".tar", ".rar",
    ".img", ".iso", ".raw", ".bin", ".dmg", ".apk", ".jar", ".ovpn", ".cpio",
];

const BINARY_EXTENSIONS: &[&str] = &[
    "7z", "apk", "bin", "cc", "class", "cpio", "dmg", "dll", "elf", "exe", "img", "iso", "jar",
    "ko", "o", "obj", "ovpn", "pkg", "pyc", "raw", "so", "sys",
];

const BINARY_FOLDER_HINTS: &[&str] = &["/bin/", "/usr/bin/", "/sbin/", "/usr/sbin/", "/lib/"];

const DOSSIER_NAME_TOKEN_WEIGHT: f64 = 1.0;
const DOSSIER_STEM_TOKEN_WEIGHT: f64 = 0.35;
const DOSSIER_EXTENSION_TOKEN_WEIGHT: f64 = 0.2;
const DOSSIER_EXTENSION_STEM_TOKEN_WEIGHT: f64 = 0.55;
const DOSSIER_HASH_TOKEN_WEIGHT: f64 = 2.5;
const DOSSIER_NORMALIZED_NAME_TOKEN_WEIGHT: f64 = 0.85;
const DOSSIER_NORMALIZED_FOLDER_TOKEN_WEIGHT: f64 = 0.15;
const DOSSIER_BINARYITY_TOKEN_WEIGHT: f64 = 0.4;
const DOSSIER_BINARY_SIGNATURE_TOKEN_WEIGHT: f64 = 0.32;
const DOSSIER_BINARY_DESCRIPTOR_TOKEN_WEIGHT: f64 = 0.03;
const DOSSIER_ARCHIVE_TOKEN_WEIGHT: f64 = 0.35;
const DOSSIER_ARCHIVE_SIGNATURE_TOKEN_WEIGHT: f64 = 0.22;
const DOSSIER_TEXT_SIGNATURE_TOKEN_WEIGHT: f64 = 0.3;
const DOSSIER_LANGUAGE_TOKEN_WEIGHT: f64 = 0.28;
const DOSSIER_SIZE_CLASS_TOKEN_WEIGHT: f64 = 0.18;
const DOSSIER_FOLDER_TOKEN_WEIGHT: f64 = 0.1;
const DOSSIER_FOLDER_PREFIX_TOKEN_WEIGHT: f64 = 0.05;
const DOSSIER_FOLDER_DEPTH_TOKEN_WEIGHT: f64 = 0.02;
const DESCRIPTOR_MAX_COMPONENT_LEN: usize = 48;
const DESCRIPTOR_MAX_COMPOSITE_LEN: usize = 192;
const DESCRIPTOR_MIN_TOKEN_LEN: usize = 2;
const DESCRIPTOR_MAX_KEY_TOKEN_LEN: usize = 32;

#[derive(Parser)]
#[command(
    name = "nightindex",
    alias = "ndex",
    version = env!("CARGO_PKG_VERSION"),
    about = "Indexed recovery copy for hostile file trees",
    long_about = "Use `nightindex` for explicit commands or `ndex` as the shorter alias.",
    after_help = "Command names:\n- `nightindex` (full command name)\n- `ndex` (binary alias)\n\nRecovery aliases:\n- `nightindex dossier` (alias: `intel`)\n- `nightindex plan-copy-missing` (alias: `plan`)\n- `nightindex sync-copy-missing` (alias: `sync`)\n- `nightindex execute-copy-missing` (alias: `execute`)\n- `nightindex extract-check` (alias: `extcheck`)",
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Scan(ScanArgs),
    CompareSummary(CompareSummaryArgs),
    Brief(BriefArgs),
    #[command(
        about = "Read-only folder identity scoring",
        alias = "intel",
        long_about = "dossier identifies the most likely folder matches between two manifests when names drift.\n\nIt combines legacy tokens (filename, stem, extension, hash, and folder path) with fingerprint profile signals:\n- NF: normalized file-name signatures\n- NFP: normalized parent-folder aliases (with prefix variants)\n- BIN / TEXT: binaryity class\n- ARCH / ARCHFAM: archive family\n- ARCHSIG: archive payload signature (for multi-part formats such as tar.gz vs tar.xz)\n\nConfidence tiers:\n- Identical: high overlap with exact-name anchors and at least two shared hashes\n- Similar: moderate overlap with shared hashes or normalized-folder-alias signals\n- Possible: extension/hash/folder evidence without stronger anchors\n- Manual: otherwise\n\nExpected behavior:\n- If fingerprint tables are missing from a database, dossier degrades to legacy-only matching.\n- Scores are deterministic, read-only, and do not alter either database."
    )]
    Dossier(DossierArgs),
    #[command(about = "Compare archive-like payload families", alias = "extcheck")]
    ExtractCheck(ExtractCheckArgs),
    #[command(
        about = "Diff persisted virtual archive-member manifests",
        alias = "amdiff"
    )]
    ArchiveMemberDiff(ArchiveMemberDiffArgs),
    #[command(
        about = "Plan reconcile/merge actions from archive-member diff signals",
        alias = "amplan"
    )]
    ArchiveMemberPlan(ArchiveMemberPlanArgs),
    #[command(about = "Create a plan for missing file copy", alias = "plan")]
    PlanCopyMissing(PlanCopyMissingArgs),
    #[command(about = "Build retry plan from resume state", alias = "resume")]
    ResumePlan(ResumePlanArgs),
    ExecuteCopyMissing(ExecuteCopyMissingArgs),
    #[command(about = "Execute a previously generated copy plan", alias = "execute")]
    ExecutePlan(ExecutePlanArgs),
    #[command(
        name = "sync-copy-missing",
        about = "Plan and execute missing-file copy in one step",
        alias = "sync"
    )]
    SyncCopyMissing(SyncCopyMissingArgs),
    #[command(about = "Summarize NDJSON copy logs")]
    Logs(LogsArgs),
    #[command(about = "Summarize DB and recent copy/resume health")]
    Status(StatusArgs),
    #[command(about = "Inspect cache coverage and signature density")]
    InspectCache(InspectCacheArgs),
    #[command(about = "Build merge materialization plan from dossier action CSV")]
    MergePlan(MergePlanArgs),
    #[command(about = "Apply a previously generated merge plan")]
    MergeApply(MergeApplyArgs),
    #[command(
        name = "rclone",
        about = "Compatibility frontend for rclone-like command style",
        trailing_var_arg = true
    )]
    Rclone(CompatCopyArgs),
    #[command(
        name = "rsync",
        about = "Compatibility frontend for rsync-like command style",
        trailing_var_arg = true
    )]
    Rsync(CompatCopyArgs),
}

#[derive(Args)]
struct ScanArgs {
    #[arg(long)]
    db: PathBuf,
    #[arg(long)]
    label: String,
    #[arg(long)]
    root: PathBuf,
    #[arg(long = "exclude")]
    exclude_prefixes: Vec<String>,
    #[arg(long = "exclude-if-present")]
    exclude_if_present: Vec<String>,
    #[arg(long)]
    policy: Option<PathBuf>,
    #[arg(long)]
    hash: bool,
}

#[derive(Args)]
struct CompareSummaryArgs {
    #[arg(long = "left-db")]
    left_db: PathBuf,
    #[arg(long = "right-db")]
    right_db: PathBuf,
    #[arg(long)]
    left: String,
    #[arg(long)]
    right: String,
    #[arg(long)]
    out_json: Option<PathBuf>,
    #[arg(long)]
    out_csv: Option<PathBuf>,
}

#[derive(Args)]
struct DossierArgs {
    #[arg(long = "left-db")]
    left_db: PathBuf,
    #[arg(long = "right-db")]
    right_db: PathBuf,
    #[arg(long)]
    left: String,
    #[arg(long)]
    right: String,
    #[arg(long = "top-k", default_value_t = 10)]
    top_k: usize,
    #[arg(long = "only-action")]
    only_action: Option<DossierAction>,
    #[arg(long = "one-per-left")]
    one_per_left: bool,
    #[arg(long = "confidence", default_value_t = DossierConfidenceTier::Manual)]
    min_confidence: DossierConfidenceTier,
    #[arg(long)]
    out_json: Option<PathBuf>,
    #[arg(long)]
    out_csv: Option<PathBuf>,
    #[arg(long = "out-actions-csv")]
    out_actions_csv: Option<PathBuf>,
    #[arg(long)]
    policy: Option<PathBuf>,
}

#[derive(Args)]
struct ExtractCheckArgs {
    #[arg(long = "left-db")]
    left_db: PathBuf,
    #[arg(long = "right-db")]
    right_db: PathBuf,
    #[arg(long)]
    left: String,
    #[arg(long)]
    right: String,
    #[arg(long)]
    out_json: Option<PathBuf>,
    #[arg(long)]
    out_csv: Option<PathBuf>,
}

#[derive(Args)]
struct ArchiveMemberDiffArgs {
    #[arg(long = "left-db")]
    left_db: PathBuf,
    #[arg(long = "right-db")]
    right_db: PathBuf,
    #[arg(long)]
    left: String,
    #[arg(long)]
    right: String,
    #[arg(long)]
    out_json: Option<PathBuf>,
    #[arg(long)]
    out_csv: Option<PathBuf>,
}

#[derive(Args)]
struct ArchiveMemberPlanArgs {
    #[arg(long = "left-db")]
    left_db: PathBuf,
    #[arg(long = "right-db")]
    right_db: PathBuf,
    #[arg(long)]
    left: String,
    #[arg(long)]
    right: String,
    #[arg(long)]
    out_json: Option<PathBuf>,
    #[arg(long)]
    out_csv: Option<PathBuf>,
}

#[derive(Args)]
struct BriefArgs {
    #[arg(long = "left-db")]
    left_db: PathBuf,
    #[arg(long = "right-db")]
    right_db: PathBuf,
    #[arg(long)]
    left: String,
    #[arg(long)]
    right: String,
    #[arg(long)]
    out_json: Option<PathBuf>,
    #[arg(long)]
    out_csv: Option<PathBuf>,
}

#[derive(Args)]
struct PlanCopyMissingArgs {
    #[arg(long = "left-db")]
    left_db: PathBuf,
    #[arg(long = "right-db")]
    right_db: PathBuf,
    #[arg(long)]
    left: String,
    #[arg(long)]
    right: String,
    #[arg(long)]
    policy: Option<PathBuf>,
    #[arg(long)]
    out_json: PathBuf,
}

#[derive(Args)]
struct ResumePlanArgs {
    #[arg(long = "db")]
    db: PathBuf,
    #[arg(long = "list-sessions")]
    list_sessions: bool,
    #[arg(long = "stats")]
    stats: bool,
    #[arg(long = "prune-completed")]
    prune_completed: bool,
    #[arg(long = "dry-run-prune")]
    dry_run_prune: bool,
    #[arg(long = "vacuum")]
    vacuum: bool,
    #[arg(long = "session-id")]
    session_id: Option<String>,
    #[arg(long = "only-failed")]
    only_failed: bool,
    #[arg(long = "max-attempts")]
    max_attempts: Option<u64>,
    #[arg(long = "jsonl-out")]
    jsonl_out: Option<PathBuf>,
    #[arg(long = "out-json")]
    out_json: Option<PathBuf>,
    #[arg(long)]
    execute: bool,
    #[arg(long)]
    from: Option<PathBuf>,
    #[arg(long)]
    to: Option<PathBuf>,
    #[arg(long)]
    overwrite: bool,
    #[arg(long)]
    dry_run: bool,
    #[arg(long)]
    stop_on_error: bool,
    #[arg(long)]
    policy: Option<PathBuf>,
    #[arg(long)]
    log: Option<PathBuf>,
    #[arg(long, default_value_t = 1000)]
    progress_every: usize,
}

#[derive(Args)]
struct ExecuteCopyMissingArgs {
    #[arg(long)]
    plan: PathBuf,
    #[arg(long)]
    from: PathBuf,
    #[arg(long)]
    to: PathBuf,
    #[arg(long)]
    overwrite: bool,
    #[arg(long)]
    dry_run: bool,
    #[arg(long)]
    stop_on_error: bool,
    #[arg(long)]
    policy: Option<PathBuf>,
    #[arg(long)]
    log: Option<PathBuf>,
    #[arg(long, default_value_t = 1000)]
    progress_every: usize,
}

#[derive(Args)]
struct ExecutePlanArgs {
    #[arg(long)]
    plan: PathBuf,
    #[arg(long)]
    from: PathBuf,
    #[arg(long)]
    to: PathBuf,
    #[arg(long)]
    overwrite: bool,
    #[arg(long)]
    dry_run: bool,
    #[arg(long)]
    stop_on_error: bool,
    #[arg(long)]
    policy: Option<PathBuf>,
    #[arg(long)]
    log: Option<PathBuf>,
    #[arg(long, default_value_t = 1000)]
    progress_every: usize,
}

#[derive(Args)]
struct SyncCopyMissingArgs {
    #[arg(long = "left-db")]
    left_db: PathBuf,
    #[arg(long = "right-db")]
    right_db: PathBuf,
    #[arg(long)]
    left: String,
    #[arg(long)]
    right: String,
    #[arg(long)]
    from: PathBuf,
    #[arg(long)]
    to: PathBuf,
    #[arg(long)]
    overwrite: bool,
    #[arg(long)]
    dry_run: bool,
    #[arg(long)]
    stop_on_error: bool,
    #[arg(long)]
    policy: Option<PathBuf>,
    #[arg(long)]
    log: Option<PathBuf>,
    #[arg(long, default_value_t = 1000)]
    progress_every: usize,
    #[arg(long)]
    write_plan: Option<PathBuf>,
}

#[derive(Args)]
struct LogsArgs {
    #[arg(long = "file")]
    file: PathBuf,
    #[arg(long = "tail", default_value_t = 200)]
    tail: usize,
    #[arg(long = "failures-only")]
    failures_only: bool,
    #[arg(long = "top-errors", default_value_t = 5)]
    top_errors: usize,
    #[arg(long = "retry-jsonl-out")]
    retry_jsonl_out: Option<PathBuf>,
}

#[derive(Args)]
struct StatusArgs {
    #[arg(long = "db")]
    db: PathBuf,
    #[arg(long = "window-minutes", default_value_t = 180)]
    window_minutes: i64,
}

#[derive(Args)]
struct InspectCacheArgs {
    #[arg(long)]
    db: PathBuf,
    #[arg(long)]
    label: Option<String>,
    #[arg(long)]
    out_json: Option<PathBuf>,
}

#[derive(Args)]
struct MergePlanArgs {
    #[arg(long = "actions-csv")]
    actions_csv: PathBuf,
    #[arg(long = "imports-root")]
    imports_root: PathBuf,
    #[arg(long = "canonical-root")]
    canonical_root: PathBuf,
    #[arg(long = "policy", default_value_t = MergePolicy::Manual)]
    policy: MergePolicy,
    #[arg(long = "out-json")]
    out_json: PathBuf,
}

#[derive(Args)]
struct MergeApplyArgs {
    #[arg(long)]
    plan: PathBuf,
    #[arg(long)]
    dry_run: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
#[value(rename_all = "kebab-case")]
enum MergePolicy {
    PreferNewer,
    PreferLarger,
    KeepBoth,
    Manual,
}

impl std::fmt::Display for MergePolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Self::PreferNewer => "prefer-newer",
            Self::PreferLarger => "prefer-larger",
            Self::KeepBoth => "keep-both",
            Self::Manual => "manual",
        };
        f.write_str(value)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct MergePlan {
    schema_version: u32,
    generated_at_ns: i64,
    policy: MergePolicy,
    imports_root: String,
    canonical_root: String,
    items: Vec<MergePlanItem>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct MergePlanItem {
    left_folder: String,
    right_folder: String,
    source: String,
    destination: String,
    decision: String,
    reason: String,
}

#[derive(Debug, Deserialize)]
struct MergeActionCsvRow {
    left_folder: String,
    rank: usize,
    right_folder: String,
    confidence_tier: String,
    next_action: String,
    overlap_ratio: f64,
    shared_hash_count: usize,
    shared_normalized_file_name_count: usize,
    shared_rel_file_count: usize,
}

#[derive(Args, Clone)]
struct CompatCopyArgs {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    compat_args: Vec<String>,
}

#[derive(Debug)]
struct CompatRuntime {
    source: PathBuf,
    destination: PathBuf,
    source_trailing_slash: bool,
    destination_trailing_slash: bool,
    overwrite: bool,
    dry_run: bool,
    stop_on_error: bool,
    policy: Option<PathBuf>,
    hash: bool,
    log: Option<PathBuf>,
    progress_every: usize,
    size_only: bool,
    delete_mode: Option<DeleteMode>,
    delete_excluded: bool,
    inplace: bool,
    stats: bool,
    human_readable: bool,
    verbosity: u8,
    backup_requested: bool,
    backup_dir: Option<PathBuf>,
    exclude_prefixes: Vec<String>,
    exclude_if_present: Vec<String>,
    include_patterns: Vec<PatternSpec>,
    files_from_patterns: Vec<PatternSpec>,
    filter_exclude_patterns: Vec<PatternSpec>,
    max_age_ns: Option<i64>,
    accepted_link_flags: Vec<String>,
    unsupported_args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PatternSpec {
    pattern: String,
    dir_only: bool,
}

impl PatternSpec {
    fn parse(value: &str) -> Option<Self> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return None;
        }

        let dir_only = trimmed.ends_with('/');
        let pattern = normalize_policy_path(trimmed);
        if pattern.is_empty() {
            return None;
        }

        Some(Self { pattern, dir_only })
    }

    fn display_value(&self) -> String {
        if self.dir_only {
            format!("{}/", self.pattern)
        } else {
            self.pattern.clone()
        }
    }
}

impl std::fmt::Display for PatternSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.display_value())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeleteMode {
    Before,
    After,
}

#[derive(Debug, Clone)]
struct FileRecord {
    rel_path: String,
    file_type: String,
    size: u64,
    mtime_ns: i64,
    fast_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FileFingerprintProfile {
    normalized_name: String,
    normalized_folder: String,
    ext: String,
    is_binary: bool,
    is_archive: bool,
    archive_family: Option<String>,
    language: String,
    size_class: String,
    binary_signature: Option<String>,
    #[serde(default)]
    binary_descriptor: Option<String>,
    text_signature: Option<String>,
    archive_signature: Option<String>,
}

#[derive(Debug, Serialize)]
struct CompareSummary {
    report_schema: String,
    report_version: u32,
    left_label: String,
    right_label: String,
    left_files: usize,
    right_files: usize,
    same_path_same_meta: usize,
    same_path_changed: usize,
    left_only: usize,
    right_only: usize,
    cache_metrics: ReportCacheMetrics,
}

#[derive(Debug, Serialize)]
struct BriefSummary {
    report_schema: String,
    report_version: u32,
    left_label: String,
    right_label: String,
    left_files: usize,
    right_files: usize,
    same_path_same_meta: usize,
    same_path_changed: usize,
    left_only: usize,
    right_only: usize,
    files_to_copy: usize,
    bytes_to_copy: u64,
    prior_bytes_per_second: Option<f64>,
    estimated_seconds: Option<u64>,
}

#[derive(Debug, Serialize, Clone)]
struct ExtractCheckEntry {
    path: String,
    folder: String,
    stem: String,
    virtual_path: String,
    virtual_member: String,
    archive_family: Option<String>,
    payload_signature: Option<String>,
    archive_depth: usize,
    size: u64,
    mtime_ns: i64,
    fast_hash: Option<String>,
}

#[derive(Debug, Serialize)]
struct ExtractCheckMatch {
    left_path: String,
    right_path: String,
    left_folder: String,
    right_folder: String,
    stem: String,
    virtual_path: String,
    archive_family: Option<String>,
    payload_signature: Option<String>,
    archive_depth: usize,
    left_size: u64,
    right_size: u64,
    left_mtime_ns: i64,
    right_mtime_ns: i64,
    left_fast_hash: Option<String>,
    right_fast_hash: Option<String>,
}

#[derive(Debug, Serialize)]
struct ExtractCheckReport {
    report_schema: String,
    report_version: u32,
    left_label: String,
    right_label: String,
    left_archive_count: usize,
    right_archive_count: usize,
    exact_matches: usize,
    left_only_count: usize,
    right_only_count: usize,
    stem_matches: usize,
    left_only_folders: Vec<String>,
    right_only_folders: Vec<String>,
    left_only: Vec<ExtractCheckEntry>,
    right_only: Vec<ExtractCheckEntry>,
    matched_by_stem: Vec<ExtractCheckMatch>,
}

#[derive(Debug, Serialize, Clone)]
struct ArchiveMemberDiffEntry {
    rel_path: String,
    virtual_member: String,
    archive_family: Option<String>,
    payload_signature: Option<String>,
    archive_depth: usize,
    size: u64,
    mtime_ns: i64,
    fast_hash: Option<String>,
}

#[derive(Debug, Serialize)]
struct ArchiveMemberDiffReport {
    report_schema: String,
    report_version: u32,
    left_label: String,
    right_label: String,
    left_members: usize,
    right_members: usize,
    exact_member_matches: usize,
    payload_family_matches: usize,
    left_only_count: usize,
    right_only_count: usize,
    left_only: Vec<ArchiveMemberDiffEntry>,
    right_only: Vec<ArchiveMemberDiffEntry>,
}

#[derive(Debug, Serialize, Clone)]
struct ArchiveMemberPlanRow {
    row_schema: String,
    row_version: u32,
    action_class: String,
    side: String,
    virtual_member: String,
    rel_path: String,
    archive_family: Option<String>,
    payload_signature: Option<String>,
    archive_depth: usize,
    size: u64,
    mtime_ns: i64,
    fast_hash: Option<String>,
    signal: String,
}

#[derive(Debug, Serialize)]
struct ArchiveMemberPlanReport {
    report_schema: String,
    report_version: u32,
    left_label: String,
    right_label: String,
    source_report_schema: String,
    source_report_version: u32,
    rows: Vec<ArchiveMemberPlanRow>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct CopyPlan {
    mode: String,
    left_label: String,
    right_label: String,
    left_db: Option<String>,
    right_db: Option<String>,
    generated_at_ns: i64,
    summary: CopyPlanSummary,
    items: Vec<CopyPlanItem>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct CopyPlanSummary {
    files_to_copy: usize,
    bytes_to_copy: u64,
    left_files: usize,
    right_files: usize,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct CopyPlanItem {
    rel_path: String,
    #[serde(default)]
    file_type: String,
    size: u64,
    mtime_ns: i64,
    fast_hash: Option<String>,
}

struct CopyRunArgs {
    source_root: PathBuf,
    destination_root: PathBuf,
    backup_dir: Option<PathBuf>,
    overwrite: bool,
    dry_run: bool,
    stop_on_error: bool,
    log: Option<PathBuf>,
    progress_every: usize,
    size_only: bool,
    hash: bool,
    copy_links_as_files: bool,
}

struct DeleteRunArgs {
    destination_root: PathBuf,
    backup_dir: Option<PathBuf>,
    dry_run: bool,
    stop_on_error: bool,
    log: Option<PathBuf>,
    progress_every: usize,
    delete_excluded: bool,
}

enum RegularCopyOutcome {
    Copied(u64),
    LostRace,
}

#[derive(Debug, Serialize, Deserialize)]
struct ExcludePolicy {
    #[serde(default)]
    #[serde(alias = "paths")]
    #[serde(alias = "prefix")]
    #[serde(alias = "exclude_prefixes")]
    directory_prefixes: Vec<String>,
    #[serde(default)]
    #[serde(alias = "tokens")]
    #[serde(alias = "folder_name_additions")]
    #[serde(alias = "folder_overrides")]
    #[serde(alias = "ignore_tokens")]
    #[serde(alias = "noise_folders")]
    #[serde(alias = "noise_dirs")]
    folder_name_additions: Vec<String>,
    #[serde(default)]
    #[serde(alias = "overrides")]
    #[serde(alias = "subtree_rules")]
    subtree_overrides: HashMap<String, Vec<String>>,
    #[serde(skip)]
    enabled: bool,
}

impl ExcludePolicy {
    fn empty() -> Self {
        Self {
            directory_prefixes: Vec::new(),
            folder_name_additions: Vec::new(),
            subtree_overrides: HashMap::new(),
            enabled: false,
        }
    }
}

#[derive(Debug, Serialize)]
struct CopyExecutionSummary {
    mode: String,
    dry_run: bool,
    overwrite: bool,
    left_label: String,
    right_label: String,
    planned_files: usize,
    copied_files: usize,
    skipped_existing: usize,
    skipped_conflict: usize,
    overwritten_files: usize,
    missing_source: usize,
    failed_files: usize,
    copied_bytes: u64,
    deleted_files: usize,
    deleted_bytes: u64,
}

#[derive(Debug, Default)]
struct DeleteExecutionSummary {
    deleted_files: usize,
    deleted_bytes: u64,
    failed_files: usize,
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum CopyEventAction {
    SourceMissing,
    SkipExisting,
    SkipConflict,
    Copy,
    Overwrite,
    Delete,
    Fail,
}

#[derive(Debug, Serialize)]
struct CopyEvent {
    schema_version: u32,
    rel_path: String,
    action: CopyEventAction,
    existing_bytes: Option<u64>,
    bytes: u64,
    dry_run: bool,
    overwrite: bool,
    reason: Option<String>,
}

#[derive(Debug, Serialize)]
struct FolderSignature {
    path: String,
    files: usize,
    total_bytes: u64,
    total_weight: f64,
    tokens: HashMap<String, f64>,
}

#[derive(Clone, Debug, Serialize)]
struct DossierMatch {
    left_folder: String,
    right_folder: String,
    overlap_weight: f64,
    left_weight: f64,
    right_weight: f64,
    overlap_ratio: f64,
    shared_rel_file_count: usize,
    shared_exact_file_name_count: usize,
    shared_normalized_file_name_count: usize,
    shared_file_stem_count: usize,
    shared_file_ext_count: usize,
    shared_ext_stem_count: usize,
    shared_hash_count: usize,
    shared_folder_token_count: usize,
    shared_normalized_parent_folder_count: usize,
    shared_binaryity_count: usize,
    shared_archive_family_count: usize,
    shared_language_count: usize,
    shared_size_class_count: usize,
    confidence_tier: DossierConfidenceTier,
}

#[derive(Debug, Serialize)]
struct DossierReport {
    report_schema: String,
    report_version: u32,
    left_db: String,
    right_db: String,
    left_label: String,
    right_label: String,
    top_k: usize,
    min_confidence: DossierConfidenceTier,
    only_action: Option<DossierAction>,
    left_folder_count: usize,
    right_folder_count: usize,
    archive_signal_candidates: usize,
    archive_signal_ratio: f64,
    cache_metrics: ReportCacheMetrics,
    left_profile_cache: CacheUsageCounters,
    right_profile_cache: CacheUsageCounters,
    confidence_counts: DossierConfidenceCounts,
    candidates: Vec<DossierMatch>,
}

#[derive(Debug, Serialize, Default, Clone, Copy, PartialEq, Eq)]
struct ReportCacheMetrics {
    left_profile_cache: CacheUsageCounters,
    right_profile_cache: CacheUsageCounters,
}

#[derive(Debug, Serialize, Default, Clone, Copy, PartialEq, Eq)]
struct CacheUsageCounters {
    hits: usize,
    misses: usize,
    analytics: CacheUsageAnalytics,
}

#[derive(Debug, Serialize, Default, Clone, Copy, PartialEq)]
struct CacheUsageAnalytics {
    coverage: CacheCoverageMetrics,
    descriptor_density: CacheDescriptorDensityMetrics,
}

#[derive(Debug, Serialize, Default, Clone, Copy, PartialEq)]
struct CacheCoverageMetrics {
    total_rows: usize,
    profile_rows: usize,
    coverage_ratio: f64,
}

#[derive(Debug, Serialize, Default, Clone, Copy, PartialEq)]
struct CacheDescriptorDensityMetrics {
    profiled_rows: usize,
    with_binary_descriptor: usize,
    with_text_signature: usize,
    with_archive_signature: usize,
    with_any_descriptor: usize,
    binary_descriptor_ratio: f64,
    text_signature_ratio: f64,
    archive_signature_ratio: f64,
    any_descriptor_ratio: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
#[value(rename_all = "lowercase")]
enum DossierAction {
    Apply,
    Review,
    Manual,
}

#[derive(Debug, Serialize, Default, Clone)]
struct DossierConfidenceCounts {
    manual: usize,
    possible: usize,
    similar: usize,
    identical: usize,
}

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ValueEnum,
)]
#[serde(rename_all = "lowercase")]
#[value(rename_all = "lowercase")]
enum DossierConfidenceTier {
    #[default]
    Manual,
    Possible,
    Similar,
    Identical,
}

impl DossierConfidenceTier {
    fn should_emit(self, minimum: DossierConfidenceTier) -> bool {
        self >= minimum
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Possible => "possible",
            Self::Similar => "similar",
            Self::Identical => "identical",
        }
    }

    fn next_action(self) -> &'static str {
        match self {
            Self::Identical => "promote with high confidence",
            Self::Similar => "review and likely apply",
            Self::Possible => "review before applying",
            Self::Manual => "manual inspection required",
        }
    }

    fn action(self) -> DossierAction {
        match self {
            Self::Identical => DossierAction::Apply,
            Self::Similar | Self::Possible => DossierAction::Review,
            Self::Manual => DossierAction::Manual,
        }
    }
}

impl DossierConfidenceCounts {
    fn bump(&mut self, tier: DossierConfidenceTier) {
        match tier {
            DossierConfidenceTier::Manual => self.manual += 1,
            DossierConfidenceTier::Possible => self.possible += 1,
            DossierConfidenceTier::Similar => self.similar += 1,
            DossierConfidenceTier::Identical => self.identical += 1,
        }
    }
}

impl std::fmt::Display for DossierConfidenceTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

const DOSSIER_CONFIDENCE_IDENTICAL_RATIO: f64 = 0.80;
const DOSSIER_CONFIDENCE_SIMILAR_RATIO: f64 = 0.35;
const DOSSIER_CONFIDENCE_POSSIBLE_RATIO: f64 = 0.15;
const BINARY_DESCRIPTOR_MAX_SAMPLE_FILE_BYTES: u64 = 64 * 1024 * 1024;
const BINARY_DESCRIPTOR_SAMPLE_POINTS: usize = 4;
const BINARY_DESCRIPTOR_SAMPLE_CHUNK_BYTES: usize = 256;

fn dossier_confidence_tier(match_record: &DossierMatch) -> DossierConfidenceTier {
    if match_record.overlap_ratio >= DOSSIER_CONFIDENCE_IDENTICAL_RATIO
        && match_record.shared_hash_count >= 2
        && match_record.shared_normalized_file_name_count >= 1
    {
        return DossierConfidenceTier::Identical;
    }

    if match_record.overlap_ratio >= DOSSIER_CONFIDENCE_SIMILAR_RATIO
        && (match_record.shared_hash_count >= 1
            || match_record.shared_normalized_parent_folder_count >= 1)
    {
        return DossierConfidenceTier::Similar;
    }

    if match_record.overlap_ratio >= DOSSIER_CONFIDENCE_POSSIBLE_RATIO
        && (match_record.shared_file_ext_count > 0
            || match_record.shared_ext_stem_count > 0
            || match_record.shared_rel_file_count > 0
            || match_record.shared_hash_count > 0
            || match_record.shared_folder_token_count > 0
            || match_record.shared_normalized_parent_folder_count > 0)
    {
        return DossierConfidenceTier::Possible;
    }

    DossierConfidenceTier::Manual
}

#[derive(Default)]
struct DossierMatchState {
    shared_weight: f64,
    shared_file_name_weight: f64,
    shared_normalized_file_name_weight: f64,
    shared_normalized_parent_folder_weight: f64,
    shared_normalized_parent_folder_count: usize,
    shared_file_stem_weight: f64,
    shared_file_stem_count: usize,
    shared_file_ext_weight: f64,
    shared_file_ext_count: usize,
    shared_ext_stem_weight: f64,
    shared_ext_stem_count: usize,
    shared_hash_weight: f64,
    shared_hash_count: usize,
    shared_folder_weight: f64,
    shared_folder_count: usize,
    shared_binaryity_weight: f64,
    shared_binaryity_count: usize,
    shared_archive_family_weight: f64,
    shared_archive_family_count: usize,
    shared_archive_signature_weight: f64,
    shared_archive_signature_count: usize,
    shared_file_name_count: usize,
    shared_normalized_file_name_count: usize,
    shared_language_count: usize,
    shared_language_weight: f64,
    shared_size_class_count: usize,
    shared_size_class_weight: f64,
    shared_rel_file_count: usize,
}

#[derive(Copy, Clone)]
enum DossierTokenFamily {
    ExactFileName,
    NormalizedFileName,
    FileStem,
    FileExtension,
    ExtensionStem,
    Hash,
    Binaryity,
    ArchiveFamily,
    ArchiveSignature,
    Language,
    SizeClass,
    NormalizedFolder,
    Folder,
    Other,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Scan(args) => scan_command(args),
        Commands::CompareSummary(args) => compare_summary_command(args),
        Commands::Brief(args) => brief_command(args),
        Commands::Dossier(args) => dossier_command(args),
        Commands::ExtractCheck(args) => extract_check_command(args),
        Commands::ArchiveMemberDiff(args) => archive_member_diff_command(args),
        Commands::ArchiveMemberPlan(args) => archive_member_plan_command(args),
        Commands::PlanCopyMissing(args) => plan_copy_missing_command(args),
        Commands::ResumePlan(args) => resume_plan_command(args),
        Commands::ExecuteCopyMissing(args) => execute_copy_missing_command(args),
        Commands::ExecutePlan(args) => execute_plan_command(args),
        Commands::SyncCopyMissing(args) => sync_copy_missing_command(args),
        Commands::Logs(args) => logs_command(args),
        Commands::Status(args) => status_command(args),
        Commands::InspectCache(args) => inspect_cache_command(args),
        Commands::MergePlan(args) => merge_plan_command(args),
        Commands::MergeApply(args) => merge_apply_command(args),
        Commands::Rclone(args) => compat_copy_command(args, "rclone"),
        Commands::Rsync(args) => compat_copy_command(args, "rsync"),
    }
}

fn scan_command(args: ScanArgs) -> Result<()> {
    let root = args
        .root
        .canonicalize()
        .with_context(|| format!("failed to resolve root {}", args.root.display()))?;
    if !root.is_dir() {
        bail!("scan root is not a directory: {}", root.display());
    }

    let exclude_prefixes = normalize_excludes(&args.exclude_prefixes);
    let exclude_if_present = normalize_excludes(&args.exclude_if_present);
    let mut policy = load_exclude_policy(args.policy.as_deref())?;
    for prefix in normalize_excludes(&args.exclude_prefixes) {
        if !policy
            .directory_prefixes
            .iter()
            .any(|existing| existing == &prefix)
        {
            policy.directory_prefixes.push(prefix);
        }
    }
    if !policy.directory_prefixes.is_empty()
        || !policy.folder_name_additions.is_empty()
        || !policy.subtree_overrides.is_empty()
    {
        policy.enabled = true;
    }
    let conn = open_db(&args.db)?;
    let mut scanned_at = now_ns()?;
    let prior_scanned_at = conn.query_row(
        "SELECT MAX(scanned_at) FROM files WHERE label = ?1",
        params![&args.label],
        |row| row.get::<_, Option<i64>>(0),
    )?;
    if let Some(previous) = prior_scanned_at {
        if scanned_at <= previous {
            scanned_at = previous + 1;
        }
    }

    println!(
        "[scan] label={} root={} hash={}",
        args.label,
        root.display(),
        args.hash
    );
    if !exclude_prefixes.is_empty() {
        println!("[scan] excludes={}", exclude_prefixes.join(", "));
    }
    if !exclude_if_present.is_empty() {
        println!(
            "[scan] exclude-if-present markers={}",
            exclude_if_present.join(", ")
        );
    }

    let mut files_seen = 0usize;
    let mut hashed = 0usize;
    let mut reused = 0usize;
    let mut excluded = 0usize;
    let mut errors = 0usize;
    let mut symlinks_seen = 0usize;
    let mut fingerprint_reused = 0usize;
    let mut fingerprint_cache_hits = 0usize;
    let mut fingerprint_cache_misses = 0usize;
    let mut fingerprint_recomputed = 0usize;

    for entry in WalkDir::new(&root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| should_walk(entry.path(), &root, &policy, &exclude_if_present))
    {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                eprintln!("[scan] walk failed: {err}");
                errors += 1;
                continue;
            }
        };

        let entry_file_type = entry.file_type();
        if entry_file_type.is_dir() {
            continue;
        }
        if !entry_file_type.is_file() && !entry_file_type.is_symlink() {
            continue;
        }
        if entry_file_type.is_symlink() {
            symlinks_seen += 1;
        }

        let rel_path = match entry.path().strip_prefix(&root) {
            Ok(path) => path_to_slash(path),
            Err(err) => {
                eprintln!(
                    "[scan] strip-prefix failed: {}: {err}",
                    entry.path().display()
                );
                errors += 1;
                continue;
            }
        };

        if should_exclude_path(&rel_path, &policy) {
            excluded += 1;
            continue;
        }

        let file_type = if entry_file_type.is_symlink() {
            "symlink"
        } else {
            "file"
        }
        .to_string();

        let metadata = match if entry_file_type.is_symlink() {
            fs::symlink_metadata(entry.path())
        } else {
            fs::metadata(entry.path())
        } {
            Ok(metadata) => metadata,
            Err(err) => {
                eprintln!("[scan] stat failed: {}: {err}", entry.path().display());
                errors += 1;
                continue;
            }
        };

        let (size, mtime_ns, source_fast_hash) = if entry_file_type.is_symlink() {
            let target = match fs::read_link(entry.path()) {
                Ok(target) => target,
                Err(err) => {
                    eprintln!("[scan] read-link failed: {}: {err}", entry.path().display());
                    errors += 1;
                    continue;
                }
            };
            let mtime_ns = metadata
                .modified()
                .ok()
                .and_then(system_time_to_ns)
                .unwrap_or_default();
            (0, mtime_ns, Some(target.to_string_lossy().to_string()))
        } else {
            let size = metadata.len();
            let mtime_ns = metadata
                .modified()
                .ok()
                .and_then(system_time_to_ns)
                .unwrap_or_default();
            (size, mtime_ns, None)
        };

        let existing = conn
            .query_row(
                "SELECT file_type, size, mtime_ns, fast_hash FROM files WHERE label = ?1 AND rel_path = ?2",
                params![&args.label, &rel_path],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, u64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .optional()?;

        let file_meta_unchanged =
            existing
                .as_ref()
                .is_some_and(|(old_file_type, old_size, old_mtime_ns, _)| {
                    *old_file_type == file_type && *old_size == size && *old_mtime_ns == mtime_ns
                });

        let fast_hash = if let Some((old_file_type, old_size, old_mtime_ns, old_hash)) = existing {
            if file_type == "symlink" {
                if old_file_type == file_type
                    && old_size == size
                    && old_mtime_ns == mtime_ns
                    && old_hash == source_fast_hash
                {
                    reused += 1;
                    old_hash
                } else {
                    source_fast_hash
                }
            } else if old_file_type == file_type
                && old_size == size
                && old_mtime_ns == mtime_ns
                && (!args.hash || old_hash.is_some())
            {
                reused += 1;
                old_hash
            } else if args.hash {
                hashed += 1;
                Some(blake3_file(entry.path())?)
            } else {
                None
            }
        } else if file_type == "symlink" {
            source_fast_hash
        } else if args.hash {
            hashed += 1;
            Some(blake3_file(entry.path())?)
        } else {
            None
        };

        let existing_profile = if file_meta_unchanged {
            conn.query_row(
                "SELECT normalized_name, normalized_folder, ext, is_binary, is_archive, archive_family, language, size_class, binary_signature, binary_descriptor, text_signature, archive_signature FROM file_fingerprints WHERE label = ?1 AND rel_path = ?2",
                params![&args.label, &rel_path],
                |row| {
                    let is_binary: i64 = row.get(3)?;
                    let is_archive: i64 = row.get(4)?;
                    Ok(FileFingerprintProfile {
                        normalized_name: row.get(0)?,
                        normalized_folder: row.get(1)?,
                        ext: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                        is_binary: is_binary != 0,
                        is_archive: is_archive != 0,
                        archive_family: row.get(5)?,
                        language: row.get(6)?,
                        size_class: row.get(7)?,
                        binary_signature: row.get(8)?,
                        binary_descriptor: row.get(9)?,
                        text_signature: row.get(10)?,
                        archive_signature: row.get(11)?,
                    })
                },
            )
            .optional()?
        } else {
            None
        };

        let fingerprint_profile = if let Some(profile) = existing_profile {
            fingerprint_reused += 1;
            profile
        } else if let Some(profile) = load_cached_file_fingerprint_profile(
            &conn,
            &rel_path,
            &file_type,
            size,
            mtime_ns,
            fast_hash.as_deref(),
        )? {
            fingerprint_cache_hits += 1;
            profile
        } else {
            fingerprint_cache_misses += 1;
            fingerprint_recomputed += 1;
            let mut profile =
                build_file_fingerprint_profile(&rel_path, &file_type, size, fast_hash.as_deref());
            if profile.is_binary {
                let sampled_signature = infer_binary_sample_signature_from_file(entry.path(), size);
                profile.binary_descriptor = Some(infer_binary_descriptor(
                    &rel_path,
                    Some(&profile.ext),
                    profile.archive_family.as_deref(),
                    &profile.size_class,
                    fast_hash.as_deref(),
                    sampled_signature.as_deref(),
                ));
            }
            if let Some(text_signature) =
                infer_semantic_text_signature_from_file(entry.path(), &profile)?
            {
                profile.text_signature = Some(text_signature);
            }
            store_cached_file_fingerprint_profile(
                &conn,
                &rel_path,
                &file_type,
                size,
                mtime_ns,
                fast_hash.as_deref(),
                &profile,
                scanned_at,
            )?;
            profile
        };

        conn.execute(
            r#"
            INSERT INTO files(label, rel_path, file_type, size, mtime_ns, fast_hash, scanned_at)
            VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ON CONFLICT(label, rel_path) DO UPDATE SET
                file_type = excluded.file_type,
                size = excluded.size,
                mtime_ns = excluded.mtime_ns,
                fast_hash = excluded.fast_hash,
                scanned_at = excluded.scanned_at
            "#,
            params![
                &args.label,
                &rel_path,
                &file_type,
                size,
                mtime_ns,
                fast_hash,
                scanned_at
            ],
        )?;

        conn.execute(
            r#"
            INSERT INTO file_fingerprints(
                label,
                rel_path,
                normalized_name,
                normalized_folder,
                ext,
                is_binary,
                is_archive,
                archive_family,
                language,
                size_class,
                binary_signature,
                binary_descriptor,
                text_signature,
                archive_signature,
                scanned_at
            )
            VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
            ON CONFLICT(label, rel_path) DO UPDATE SET
                normalized_name = excluded.normalized_name,
                normalized_folder = excluded.normalized_folder,
                ext = excluded.ext,
                is_binary = excluded.is_binary,
                is_archive = excluded.is_archive,
                archive_family = excluded.archive_family,
                language = excluded.language,
                size_class = excluded.size_class,
                binary_signature = excluded.binary_signature,
                binary_descriptor = excluded.binary_descriptor,
                text_signature = excluded.text_signature,
                archive_signature = excluded.archive_signature,
                scanned_at = excluded.scanned_at
            "#,
            params![
                &args.label,
                &rel_path,
                &fingerprint_profile.normalized_name,
                &fingerprint_profile.normalized_folder,
                &fingerprint_profile.ext,
                i32::from(fingerprint_profile.is_binary),
                i32::from(fingerprint_profile.is_archive),
                &fingerprint_profile.archive_family,
                &fingerprint_profile.language,
                &fingerprint_profile.size_class,
                &fingerprint_profile.binary_signature,
                &fingerprint_profile.binary_descriptor,
                &fingerprint_profile.text_signature,
                &fingerprint_profile.archive_signature,
                scanned_at
            ],
        )?;

        persist_virtual_archive_member(
            &conn,
            &args.label,
            &rel_path,
            size,
            mtime_ns,
            fast_hash.as_deref(),
            scanned_at,
        )?;

        files_seen += 1;
        if files_seen % 500 == 0 {
            println!(
                "[scan] files={} hashed={} reused={} fp_reused={} fp_cache_hits={} fp_cache_misses={} fp_recomputed={} excluded={} errors={}",
                files_seen,
                hashed,
                reused,
                fingerprint_reused,
                fingerprint_cache_hits,
                fingerprint_cache_misses,
                fingerprint_recomputed,
                excluded,
                errors
            );
        }
    }

    conn.execute(
        "DELETE FROM virtual_archive_members WHERE label = ?1 AND scanned_at < ?2",
        params![&args.label, scanned_at],
    )?;

    println!(
        "[scan] summary: files={} symlinks={} hashed={} reused={} fp_reused={} fp_cache_hits={} fp_cache_misses={} fp_recomputed={} excluded={} errors={}",
        files_seen,
        symlinks_seen,
        hashed,
        reused,
        fingerprint_reused,
        fingerprint_cache_hits,
        fingerprint_cache_misses,
        fingerprint_recomputed,
        excluded,
        errors
    );
    eprintln!(
        "[scan] cache-summary: fp_cache_hits={} fp_cache_misses={} fp_reused={} fp_recomputed={}",
        fingerprint_cache_hits,
        fingerprint_cache_misses,
        fingerprint_reused,
        fingerprint_recomputed
    );
    Ok(())
}

fn compare_summary_command(args: CompareSummaryArgs) -> Result<()> {
    let left_conn = open_db(&args.left_db)?;
    let right_conn = open_db(&args.right_db)?;

    let left_rows = load_label(&left_conn, &args.left)?;
    let right_rows = load_label(&right_conn, &args.right)?;
    let left_profiles = load_file_fingerprint_profiles(&left_conn, &args.left)?;
    let right_profiles = load_file_fingerprint_profiles(&right_conn, &args.right)?;
    let cache_metrics = ReportCacheMetrics {
        left_profile_cache: profile_cache_usage(&left_rows, &left_profiles),
        right_profile_cache: profile_cache_usage(&right_rows, &right_profiles),
    };

    let (summary, _, _) = build_compare_and_copy_summary(
        &left_rows,
        &right_rows,
        &args.left,
        &args.right,
        cache_metrics,
    )?;

    let json = serde_json::to_string_pretty(&summary)?;
    println!("{json}");
    if let Some(path) = args.out_json {
        std::fs::write(&path, format!("{json}\n"))
            .with_context(|| format!("failed to write {}", path.display()))?;
    }
    if let Some(path) = args.out_csv {
        let csv = build_compare_summary_csv(&summary);
        std::fs::write(&path, csv)
            .with_context(|| format!("failed to write {}", path.display()))?;
    }
    Ok(())
}

fn brief_command(args: BriefArgs) -> Result<()> {
    let left_conn = open_db(&args.left_db)?;
    let right_conn = open_db(&args.right_db)?;

    let left_rows = load_label(&left_conn, &args.left)?;
    let right_rows = load_label(&right_conn, &args.right)?;

    let (summary, files_to_copy, bytes_to_copy) = build_compare_and_copy_summary(
        &left_rows,
        &right_rows,
        &args.left,
        &args.right,
        ReportCacheMetrics::default(),
    )?;

    let left_sample = latest_copy_run_sample(&left_conn, &args.left, &args.right)?;
    let right_sample = latest_copy_run_sample(&right_conn, &args.left, &args.right)?;
    let prior_sample = match (left_sample, right_sample) {
        (Some(left), Some(right)) => {
            if left.copied_at >= right.copied_at {
                Some(left)
            } else {
                Some(right)
            }
        }
        (Some(sample), None) => Some(sample),
        (None, Some(sample)) => Some(sample),
        (None, None) => None,
    };

    let (prior_bytes_per_second, estimated_seconds) =
        estimate_copy_eta(prior_sample.as_ref(), bytes_to_copy);

    let brief = BriefSummary {
        report_schema: "nightindex.brief".to_string(),
        report_version: 1,
        left_label: summary.left_label,
        right_label: summary.right_label,
        left_files: summary.left_files,
        right_files: summary.right_files,
        same_path_same_meta: summary.same_path_same_meta,
        same_path_changed: summary.same_path_changed,
        left_only: summary.left_only,
        right_only: summary.right_only,
        files_to_copy,
        bytes_to_copy,
        prior_bytes_per_second,
        estimated_seconds,
    };

    let json = serde_json::to_string_pretty(&brief)?;
    println!("{json}");
    if let Some(path) = args.out_json {
        std::fs::write(&path, format!("{json}\n"))
            .with_context(|| format!("failed to write {}", path.display()))?;
    }
    if let Some(path) = args.out_csv {
        let csv = build_brief_csv(&brief);
        std::fs::write(&path, csv)
            .with_context(|| format!("failed to write {}", path.display()))?;
    }
    eprintln!(
        "[brief] summary: copy {} files / {} bytes; {} same, {} changed, {} left-only, {} right-only{}",
        brief.files_to_copy,
        brief.bytes_to_copy,
        brief.same_path_same_meta,
        brief.same_path_changed,
        brief.left_only,
        brief.right_only,
        brief
            .estimated_seconds
            .map_or_else(|| "".to_string(), |seconds| format!(", ETA ~{seconds}s"))
    );
    Ok(())
}

fn extract_check_command(args: ExtractCheckArgs) -> Result<()> {
    let left_conn = open_db(&args.left_db)?;
    let right_conn = open_db(&args.right_db)?;

    let left_rows = load_label(&left_conn, &args.left)?;
    let right_rows = load_label(&right_conn, &args.right)?;

    let mut left_archives = load_virtual_archive_entries(&left_conn, &args.left)?;
    if left_archives.is_empty() {
        left_archives = build_archive_entries(&left_rows)?;
    }
    let mut right_archives = load_virtual_archive_entries(&right_conn, &args.right)?;
    if right_archives.is_empty() {
        right_archives = build_archive_entries(&right_rows)?;
    }

    let mut left_map: HashMap<String, ExtractCheckEntry> =
        HashMap::with_capacity(left_archives.len());
    let mut right_map: HashMap<String, ExtractCheckEntry> =
        HashMap::with_capacity(right_archives.len());

    for row in left_archives {
        left_map.insert(row.path.clone(), row);
    }
    for row in right_archives {
        right_map.insert(row.path.clone(), row);
    }

    let mut exact_matches = 0usize;
    let mut left_only: Vec<ExtractCheckEntry> = Vec::new();
    let mut right_only: Vec<ExtractCheckEntry> = Vec::new();

    for (path, row) in &left_map {
        if right_map.contains_key(path) {
            exact_matches += 1;
        } else {
            left_only.push(row.clone());
        }
    }

    for (path, row) in &right_map {
        if !left_map.contains_key(path) {
            right_only.push(row.clone());
        }
    }

    let mut final_left_only = left_only;
    let mut final_right_only = right_only;
    final_left_only.sort_by(|a, b| a.path.cmp(&b.path));
    final_right_only.sort_by(|a, b| a.path.cmp(&b.path));

    let mut right_by_stem: HashMap<String, Vec<ExtractCheckEntry>> = HashMap::new();
    for entry in &final_right_only {
        right_by_stem
            .entry(entry.stem.clone())
            .or_default()
            .push(entry.clone());
    }
    for candidates in right_by_stem.values_mut() {
        candidates.sort_by(|a, b| a.path.cmp(&b.path));
    }

    let mut matched_by_stem = Vec::new();
    let mut unmatched_left_only = Vec::new();
    for left_entry in final_left_only {
        if let Some(candidates) = right_by_stem.get_mut(&left_entry.stem) {
            if let Some(right_entry) = candidates.pop() {
                matched_by_stem.push(ExtractCheckMatch {
                    left_path: left_entry.path,
                    right_path: right_entry.path,
                    left_folder: left_entry.folder,
                    right_folder: right_entry.folder,
                    stem: left_entry.stem,
                    virtual_path: left_entry.virtual_path,
                    archive_family: left_entry.archive_family,
                    payload_signature: left_entry.payload_signature,
                    archive_depth: left_entry.archive_depth,
                    left_size: left_entry.size,
                    right_size: right_entry.size,
                    left_mtime_ns: left_entry.mtime_ns,
                    right_mtime_ns: right_entry.mtime_ns,
                    left_fast_hash: left_entry.fast_hash,
                    right_fast_hash: right_entry.fast_hash,
                });
                continue;
            }
        }
        unmatched_left_only.push(left_entry);
    }

    let mut unmatched_right_only = Vec::new();
    for mut entries in right_by_stem.into_values() {
        unmatched_right_only.append(&mut entries);
    }

    unmatched_left_only.sort_by(|a, b| a.path.cmp(&b.path));
    unmatched_right_only.sort_by(|a, b| a.path.cmp(&b.path));
    matched_by_stem.sort_by(|a, b| {
        a.stem
            .cmp(&b.stem)
            .then_with(|| a.left_path.cmp(&b.left_path))
    });

    let left_only_folders = build_unique_folders(&unmatched_left_only);
    let right_only_folders = build_unique_folders(&unmatched_right_only);

    let report = ExtractCheckReport {
        report_schema: "nightindex.extract_check".to_string(),
        report_version: 1,
        left_label: args.left,
        right_label: args.right,
        left_archive_count: left_map.len(),
        right_archive_count: right_map.len(),
        exact_matches,
        left_only_count: unmatched_left_only.len(),
        right_only_count: unmatched_right_only.len(),
        stem_matches: matched_by_stem.len(),
        left_only_folders,
        right_only_folders,
        left_only: unmatched_left_only,
        right_only: unmatched_right_only,
        matched_by_stem,
    };

    let json = serde_json::to_string_pretty(&report)?;
    println!("{json}");
    if let Some(path) = args.out_json {
        std::fs::write(&path, format!("{json}\n"))
            .with_context(|| format!("failed to write {}", path.display()))?;
    }
    if let Some(path) = args.out_csv {
        let csv = build_extract_check_csv(&report);
        std::fs::write(&path, csv)
            .with_context(|| format!("failed to write {}", path.display()))?;
    }
    eprintln!(
        "[extract-check] summary: {} exact matches, {} stem matches, {} left-only entries ({} folders), {} right-only entries ({} folders)",
        report.exact_matches,
        report.stem_matches,
        report.left_only_count,
        report.left_only_folders.len(),
        report.right_only_count,
        report.right_only_folders.len()
    );
    Ok(())
}

fn archive_member_diff_command(args: ArchiveMemberDiffArgs) -> Result<()> {
    let left_conn = open_readonly_db(&args.left_db)?;
    let right_conn = open_readonly_db(&args.right_db)?;
    let report = build_archive_member_diff_report(&left_conn, &right_conn, &args.left, &args.right)?;
    let json = serde_json::to_string_pretty(&report)?;
    println!("{json}");
    if let Some(path) = args.out_json {
        std::fs::write(&path, format!("{json}\n"))
            .with_context(|| format!("failed to write {}", path.display()))?;
    }
    if let Some(path) = args.out_csv {
        let csv = build_archive_member_diff_csv(&report);
        std::fs::write(&path, csv)
            .with_context(|| format!("failed to write {}", path.display()))?;
    }
    eprintln!(
        "[archive-member-diff] summary: {} exact, {} payload-family, {} left-only, {} right-only",
        report.exact_member_matches,
        report.payload_family_matches,
        report.left_only_count,
        report.right_only_count
    );
    Ok(())
}

fn build_archive_member_diff_report(
    left_conn: &Connection,
    right_conn: &Connection,
    left_label: &str,
    right_label: &str,
) -> Result<ArchiveMemberDiffReport> {
    let mut left_entries = load_virtual_archive_entries(left_conn, left_label)?;
    if left_entries.is_empty() {
        let left_rows = load_label(left_conn, left_label)?;
        left_entries = build_archive_entries(&left_rows)?;
    }
    let mut right_entries = load_virtual_archive_entries(right_conn, right_label)?;
    if right_entries.is_empty() {
        let right_rows = load_label(right_conn, right_label)?;
        right_entries = build_archive_entries(&right_rows)?;
    }

    let mut left_map: HashMap<String, ExtractCheckEntry> = HashMap::with_capacity(left_entries.len());
    let mut right_map: HashMap<String, ExtractCheckEntry> =
        HashMap::with_capacity(right_entries.len());
    for entry in left_entries {
        left_map.insert(entry.virtual_member.clone(), entry);
    }
    for entry in right_entries {
        right_map.insert(entry.virtual_member.clone(), entry);
    }

    let mut exact_member_matches = 0usize;
    let mut payload_family_matches = 0usize;
    let mut left_only = Vec::new();
    let mut right_only = Vec::new();

    for (member, left_entry) in &left_map {
        if right_map.contains_key(member) {
            exact_member_matches += 1;
        } else {
            left_only.push(ArchiveMemberDiffEntry {
                rel_path: left_entry.path.clone(),
                virtual_member: left_entry.virtual_member.clone(),
                archive_family: left_entry.archive_family.clone(),
                payload_signature: left_entry.payload_signature.clone(),
                archive_depth: left_entry.archive_depth,
                size: left_entry.size,
                mtime_ns: left_entry.mtime_ns,
                fast_hash: left_entry.fast_hash.clone(),
            });
        }
    }
    for (member, right_entry) in &right_map {
        if !left_map.contains_key(member) {
            right_only.push(ArchiveMemberDiffEntry {
                rel_path: right_entry.path.clone(),
                virtual_member: right_entry.virtual_member.clone(),
                archive_family: right_entry.archive_family.clone(),
                payload_signature: right_entry.payload_signature.clone(),
                archive_depth: right_entry.archive_depth,
                size: right_entry.size,
                mtime_ns: right_entry.mtime_ns,
                fast_hash: right_entry.fast_hash.clone(),
            });
        }
    }

    let mut left_payloads = HashSet::new();
    for item in &left_only {
        if let (Some(fam), Some(sig)) = (&item.archive_family, &item.payload_signature) {
            left_payloads.insert(format!("{fam}|{sig}"));
        }
    }
    for item in &right_only {
        if let (Some(fam), Some(sig)) = (&item.archive_family, &item.payload_signature) {
            if left_payloads.contains(&format!("{fam}|{sig}")) {
                payload_family_matches += 1;
            }
        }
    }

    left_only.sort_by(|a, b| a.virtual_member.cmp(&b.virtual_member));
    right_only.sort_by(|a, b| a.virtual_member.cmp(&b.virtual_member));

    Ok(ArchiveMemberDiffReport {
        report_schema: ARCHIVE_MEMBER_DIFF_REPORT_SCHEMA.to_string(),
        report_version: REPORT_VERSION_V1,
        left_label: left_label.to_string(),
        right_label: right_label.to_string(),
        left_members: left_map.len(),
        right_members: right_map.len(),
        exact_member_matches,
        payload_family_matches,
        left_only_count: left_only.len(),
        right_only_count: right_only.len(),
        left_only,
        right_only,
    })
}

fn build_archive_member_plan_rows(report: &ArchiveMemberDiffReport) -> Vec<ArchiveMemberPlanRow> {
    let mut rows = Vec::new();
    let mut left_payload_keys = HashSet::new();
    let mut right_payload_keys = HashSet::new();
    for item in &report.left_only {
        if let (Some(fam), Some(sig)) = (&item.archive_family, &item.payload_signature) {
            left_payload_keys.insert(format!("{fam}|{sig}"));
        }
    }
    for item in &report.right_only {
        if let (Some(fam), Some(sig)) = (&item.archive_family, &item.payload_signature) {
            right_payload_keys.insert(format!("{fam}|{sig}"));
        }
    }

    for item in &report.left_only {
        let payload_key = item
            .archive_family
            .as_deref()
            .zip(item.payload_signature.as_deref())
            .map(|(fam, sig)| format!("{fam}|{sig}"));
        let matched_family = payload_key
            .as_ref()
            .is_some_and(|key| right_payload_keys.contains(key));
        let (action_class, signal) = if matched_family {
            ("review_payload_family_match", "payload_family_match")
        } else if item.payload_signature.is_some() {
            ("copy_left_only", "left_only")
        } else {
            ("review_conflict", "left_only_no_payload_signature")
        };
        rows.push(ArchiveMemberPlanRow {
            row_schema: ARCHIVE_MEMBER_PLAN_ROW_SCHEMA.to_string(),
            row_version: REPORT_VERSION_V1,
            action_class: action_class.to_string(),
            side: "left".to_string(),
            virtual_member: item.virtual_member.clone(),
            rel_path: item.rel_path.clone(),
            archive_family: item.archive_family.clone(),
            payload_signature: item.payload_signature.clone(),
            archive_depth: item.archive_depth,
            size: item.size,
            mtime_ns: item.mtime_ns,
            fast_hash: item.fast_hash.clone(),
            signal: signal.to_string(),
        });
    }

    for item in &report.right_only {
        let payload_key = item
            .archive_family
            .as_deref()
            .zip(item.payload_signature.as_deref())
            .map(|(fam, sig)| format!("{fam}|{sig}"));
        let matched_family = payload_key
            .as_ref()
            .is_some_and(|key| left_payload_keys.contains(key));
        let (action_class, signal) = if matched_family {
            ("review_payload_family_match", "payload_family_match")
        } else if item.payload_signature.is_some() {
            ("copy_right_only", "right_only")
        } else {
            ("review_conflict", "right_only_no_payload_signature")
        };
        rows.push(ArchiveMemberPlanRow {
            row_schema: ARCHIVE_MEMBER_PLAN_ROW_SCHEMA.to_string(),
            row_version: REPORT_VERSION_V1,
            action_class: action_class.to_string(),
            side: "right".to_string(),
            virtual_member: item.virtual_member.clone(),
            rel_path: item.rel_path.clone(),
            archive_family: item.archive_family.clone(),
            payload_signature: item.payload_signature.clone(),
            archive_depth: item.archive_depth,
            size: item.size,
            mtime_ns: item.mtime_ns,
            fast_hash: item.fast_hash.clone(),
            signal: signal.to_string(),
        });
    }

    rows.sort_by(|a, b| a.virtual_member.cmp(&b.virtual_member).then(a.side.cmp(&b.side)));
    rows
}

fn archive_member_plan_command(args: ArchiveMemberPlanArgs) -> Result<()> {
    let left_conn = open_readonly_db(&args.left_db)?;
    let right_conn = open_readonly_db(&args.right_db)?;
    let diff = build_archive_member_diff_report(&left_conn, &right_conn, &args.left, &args.right)?;
    let rows = build_archive_member_plan_rows(&diff);
    let report = ArchiveMemberPlanReport {
        report_schema: ARCHIVE_MEMBER_PLAN_REPORT_SCHEMA.to_string(),
        report_version: REPORT_VERSION_V1,
        left_label: diff.left_label.clone(),
        right_label: diff.right_label.clone(),
        source_report_schema: diff.report_schema.clone(),
        source_report_version: diff.report_version,
        rows,
    };
    let json = serde_json::to_string_pretty(&report)?;
    println!("{json}");
    if let Some(path) = args.out_json {
        std::fs::write(&path, format!("{json}\n"))
            .with_context(|| format!("failed to write {}", path.display()))?;
    }
    if let Some(path) = args.out_csv {
        let csv = build_archive_member_plan_csv(&report);
        std::fs::write(&path, csv)
            .with_context(|| format!("failed to write {}", path.display()))?;
    }
    eprintln!("[archive-member-plan] rows: {}", report.rows.len());
    Ok(())
}

fn build_archive_entries(rows: &[FileRecord]) -> Result<Vec<ExtractCheckEntry>> {
    let mut entries = Vec::new();
    for row in rows {
        if !is_archive_path(&row.rel_path) {
            continue;
        }

        let folder = folder_path_from_row(&row.rel_path);
        let stem = build_archive_stem(&row.rel_path);
        let archive_family = infer_archive_family(&row.rel_path);
        let payload_signature = archive_family
            .as_deref()
            .and_then(|family| infer_archive_payload_signature(&row.rel_path, family));
        let virtual_path = build_virtual_archive_path(&row.rel_path);
        let virtual_member = build_virtual_archive_member_path(&row.rel_path);
        let archive_depth = archive_family.as_deref().map_or(0, archive_family_depth);

        entries.push(ExtractCheckEntry {
            path: row.rel_path.clone(),
            folder,
            stem,
            virtual_path,
            virtual_member,
            archive_family,
            payload_signature,
            archive_depth,
            size: row.size,
            mtime_ns: row.mtime_ns,
            fast_hash: row.fast_hash.clone(),
        });
    }
    Ok(entries)
}

fn persist_virtual_archive_member(
    conn: &Connection,
    label: &str,
    rel_path: &str,
    size: u64,
    mtime_ns: i64,
    fast_hash: Option<&str>,
    scanned_at: i64,
) -> Result<()> {
    if !is_archive_path(rel_path) {
        conn.execute(
            "DELETE FROM virtual_archive_members WHERE label = ?1 AND rel_path = ?2",
            params![label, rel_path],
        )?;
        return Ok(());
    }

    let archive_family = infer_archive_family(rel_path);
    let payload_signature = archive_family
        .as_deref()
        .and_then(|family| infer_archive_payload_signature(rel_path, family));
    let virtual_path = build_virtual_archive_path(rel_path);
    let virtual_member = build_virtual_archive_member_path(rel_path);
    let archive_depth = archive_family.as_deref().map_or(0, archive_family_depth);

    conn.execute(
        r#"
        INSERT INTO virtual_archive_members(
            label,
            rel_path,
            virtual_path,
            virtual_member,
            archive_family,
            payload_signature,
            archive_depth,
            size,
            mtime_ns,
            fast_hash,
            scanned_at
        )
        VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
        ON CONFLICT(label, rel_path) DO UPDATE SET
            virtual_path = excluded.virtual_path,
            virtual_member = excluded.virtual_member,
            archive_family = excluded.archive_family,
            payload_signature = excluded.payload_signature,
            archive_depth = excluded.archive_depth,
            size = excluded.size,
            mtime_ns = excluded.mtime_ns,
            fast_hash = excluded.fast_hash,
            scanned_at = excluded.scanned_at
        "#,
        params![
            label,
            rel_path,
            virtual_path,
            virtual_member,
            archive_family,
            payload_signature,
            archive_depth as i64,
            size,
            mtime_ns,
            fast_hash,
            scanned_at
        ],
    )?;
    Ok(())
}

fn load_virtual_archive_entries(conn: &Connection, label: &str) -> Result<Vec<ExtractCheckEntry>> {
    let mut stmt = conn.prepare(
        "SELECT rel_path, virtual_path, virtual_member, archive_family, payload_signature, archive_depth, size, mtime_ns, fast_hash
         FROM virtual_archive_members
         WHERE label = ?1
         ORDER BY rel_path",
    )?;
    let rows = stmt.query_map(params![label], |row| {
        let path: String = row.get(0)?;
        Ok(ExtractCheckEntry {
            folder: folder_path_from_row(&path),
            stem: build_archive_stem(&path),
            path,
            virtual_path: row.get(1)?,
            virtual_member: row.get(2)?,
            archive_family: row.get(3)?,
            payload_signature: row.get(4)?,
            archive_depth: row.get::<_, i64>(5)? as usize,
            size: row.get(6)?,
            mtime_ns: row.get(7)?,
            fast_hash: row.get(8)?,
        })
    })?;

    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

fn is_archive_path(path: &str) -> bool {
    let lower_path = path.to_lowercase();
    ARCHIVE_EXTENSIONS
        .iter()
        .any(|extension| lower_path.ends_with(extension))
}

fn build_archive_stem(rel_path: &str) -> String {
    let file_name = Path::new(rel_path)
        .file_name()
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_else(|| rel_path.to_string());

    let lower_name = file_name.to_lowercase();
    let mut best_stem = Path::new(&file_name)
        .file_stem()
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_else(|| file_name.clone());
    let mut best_extension_len = 0usize;

    for extension in ARCHIVE_EXTENSIONS {
        if lower_name.ends_with(extension) {
            let stem = &file_name[..file_name.len() - extension.len()];
            if extension.len() >= best_extension_len {
                best_extension_len = extension.len();
                best_stem = stem.to_string();
            }
        }
    }

    if best_stem.ends_with('.') {
        best_stem.truncate(best_stem.len() - 1);
    }
    best_stem
}

fn build_virtual_archive_path(rel_path: &str) -> String {
    let normalized = rel_path.replace('\\', "/");
    let stem = build_archive_stem(&normalized);
    let family_path = build_archive_family_path(&normalized);
    if family_path.is_empty() {
        format!("{stem}/")
    } else {
        format!("{stem}/@{family_path}")
    }
}

fn build_virtual_archive_member_path(rel_path: &str) -> String {
    let normalized = rel_path.replace('\\', "/");
    let stem = build_archive_stem(&normalized);
    let member = normalize_virtual_archive_member_identity(&stem);
    let family_path = build_archive_family_path(&normalized);
    if family_path.is_empty() {
        format!("{member}/")
    } else {
        format!("{member}/@{family_path}")
    }
}

fn build_archive_family_path(path: &str) -> String {
    let family = infer_archive_family(path).unwrap_or_else(|| "archive".to_string());
    family
        .split(|ch| ch == '.' || ch == '+')
        .filter(|part| !part.is_empty())
        .map(normalize_archive_token)
        .collect::<Vec<_>>()
        .join("/")
}

fn normalize_virtual_archive_member_identity(value: &str) -> String {
    let normalized = value.replace('\\', "/");
    let mut segments = Vec::new();
    for segment in normalized.split('/') {
        let trimmed = segment.trim();
        if trimmed.is_empty() || trimmed == "." {
            continue;
        }
        if trimmed == ".." {
            let _ = segments.pop();
            continue;
        }
        let mut out = String::new();
        let mut prior_underscore = false;
        for ch in trimmed.chars() {
            let mapped = if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '_'
            };
            if mapped == '_' {
                if prior_underscore {
                    continue;
                }
                prior_underscore = true;
            } else {
                prior_underscore = false;
            }
            out.push(mapped);
        }
        let token = out.trim_matches('_').to_string();
        if !token.is_empty() {
            segments.push(token);
        }
    }
    if segments.is_empty() {
        "member".to_string()
    } else {
        segments.join("/")
    }
}

fn archive_family_depth(archive_family: &str) -> usize {
    archive_family
        .split(|ch| ch == '.' || ch == '+')
        .filter(|part| !part.is_empty())
        .count()
}

fn build_unique_folders(entries: &[ExtractCheckEntry]) -> Vec<String> {
    let mut folders = entries
        .iter()
        .map(|entry| entry.folder.clone())
        .collect::<Vec<_>>();
    folders.sort_unstable();
    folders.dedup();
    folders
}

fn build_extract_check_csv(report: &ExtractCheckReport) -> String {
    let mut csv = String::new();
    csv.push_str("section,left_label,right_label,metric,value\n");
    let _ = std::fmt::Write::write_fmt(
        &mut csv,
        format_args!(
            "summary,{},{},left_archive_count,{}\n",
            csv_escape(&report.left_label),
            csv_escape(&report.right_label),
            report.left_archive_count
        ),
    );
    let _ = std::fmt::Write::write_fmt(
        &mut csv,
        format_args!(
            "summary,{},{},right_archive_count,{}\n",
            csv_escape(&report.left_label),
            csv_escape(&report.right_label),
            report.right_archive_count
        ),
    );
    let _ = std::fmt::Write::write_fmt(
        &mut csv,
        format_args!(
            "summary,{},{},exact_matches,{}\n",
            csv_escape(&report.left_label),
            csv_escape(&report.right_label),
            report.exact_matches
        ),
    );
    let _ = std::fmt::Write::write_fmt(
        &mut csv,
        format_args!(
            "summary,{},{},left_only_count,{}\n",
            csv_escape(&report.left_label),
            csv_escape(&report.right_label),
            report.left_only_count
        ),
    );
    let _ = std::fmt::Write::write_fmt(
        &mut csv,
        format_args!(
            "summary,{},{},right_only_count,{}\n",
            csv_escape(&report.left_label),
            csv_escape(&report.right_label),
            report.right_only_count
        ),
    );
    let _ = std::fmt::Write::write_fmt(
        &mut csv,
        format_args!(
            "summary,{},{},stem_matches,{}\n",
            csv_escape(&report.left_label),
            csv_escape(&report.right_label),
            report.stem_matches
        ),
    );

    for folder in &report.left_only_folders {
        let _ = std::fmt::Write::write_fmt(
            &mut csv,
            format_args!(
                "left_only,{},{},folder,{}\n",
                csv_escape(&report.left_label),
                csv_escape(&report.right_label),
                csv_escape(folder)
            ),
        );
    }
    for folder in &report.right_only_folders {
        let _ = std::fmt::Write::write_fmt(
            &mut csv,
            format_args!(
                "right_only,{},{},folder,{}\n",
                csv_escape(&report.left_label),
                csv_escape(&report.right_label),
                csv_escape(folder)
            ),
        );
    }
    for entry in &report.left_only {
        let _ = std::fmt::Write::write_fmt(
            &mut csv,
            format_args!(
                "left_only_entry,{},{},path,{}\n",
                csv_escape(&report.left_label),
                csv_escape(&report.right_label),
                csv_escape(&entry.path)
            ),
        );
    }
    for entry in &report.right_only {
        let _ = std::fmt::Write::write_fmt(
            &mut csv,
            format_args!(
                "right_only_entry,{},{},path,{}\n",
                csv_escape(&report.left_label),
                csv_escape(&report.right_label),
                csv_escape(&entry.path)
            ),
        );
    }
    for entry in &report.matched_by_stem {
        let _ = std::fmt::Write::write_fmt(
            &mut csv,
            format_args!(
                "stem_match,{},{},stem,{},{}\n",
                csv_escape(&report.left_label),
                csv_escape(&report.right_label),
                csv_escape(&entry.stem),
                csv_escape(&format!("{}|{}", entry.left_path, entry.right_path))
            ),
        );
        let _ = std::fmt::Write::write_fmt(
            &mut csv,
            format_args!(
                "stem_match,{},{},virtual_path,{},{}\n",
                csv_escape(&report.left_label),
                csv_escape(&report.right_label),
                csv_escape(&entry.virtual_path),
                csv_escape(&entry.payload_signature.clone().unwrap_or_default())
            ),
        );
    }
    csv
}

fn build_archive_member_diff_csv(report: &ArchiveMemberDiffReport) -> String {
    let mut csv = String::new();
    csv.push_str("section,left_label,right_label,metric,value\n");
    let _ = std::fmt::Write::write_fmt(
        &mut csv,
        format_args!(
            "summary,{},{},left_members,{}\n",
            csv_escape(&report.left_label),
            csv_escape(&report.right_label),
            report.left_members
        ),
    );
    let _ = std::fmt::Write::write_fmt(
        &mut csv,
        format_args!(
            "summary,{},{},right_members,{}\n",
            csv_escape(&report.left_label),
            csv_escape(&report.right_label),
            report.right_members
        ),
    );
    let _ = std::fmt::Write::write_fmt(
        &mut csv,
        format_args!(
            "summary,{},{},exact_member_matches,{}\n",
            csv_escape(&report.left_label),
            csv_escape(&report.right_label),
            report.exact_member_matches
        ),
    );
    let _ = std::fmt::Write::write_fmt(
        &mut csv,
        format_args!(
            "summary,{},{},payload_family_matches,{}\n",
            csv_escape(&report.left_label),
            csv_escape(&report.right_label),
            report.payload_family_matches
        ),
    );
    for item in &report.left_only {
        let _ = std::fmt::Write::write_fmt(
            &mut csv,
            format_args!(
                "left_only,{},{},virtual_member,{}\n",
                csv_escape(&report.left_label),
                csv_escape(&report.right_label),
                csv_escape(&item.virtual_member)
            ),
        );
    }
    for item in &report.right_only {
        let _ = std::fmt::Write::write_fmt(
            &mut csv,
            format_args!(
                "right_only,{},{},virtual_member,{}\n",
                csv_escape(&report.left_label),
                csv_escape(&report.right_label),
                csv_escape(&item.virtual_member)
            ),
        );
    }
    csv
}

fn dossier_command(args: DossierArgs) -> Result<()> {
    let left_conn = open_readonly_db(&args.left_db)?;
    let right_conn = open_readonly_db(&args.right_db)?;
    let policy = load_exclude_policy(args.policy.as_deref())?;
    let left_rows = load_label(&left_conn, &args.left)?;
    let right_rows = load_label(&right_conn, &args.right)?;
    let left_profiles = load_file_fingerprint_profiles(&left_conn, &args.left)?;
    let right_profiles = load_file_fingerprint_profiles(&right_conn, &args.right)?;

    let left_rows: Vec<FileRecord> = left_rows
        .into_iter()
        .filter(|row| !should_exclude_path(&row.rel_path, &policy))
        .collect();
    let right_rows: Vec<FileRecord> = right_rows
        .into_iter()
        .filter(|row| !should_exclude_path(&row.rel_path, &policy))
        .collect();
    let left_profile_cache = profile_cache_usage(&left_rows, &left_profiles);
    let right_profile_cache = profile_cache_usage(&right_rows, &right_profiles);
    let left_signatures = build_folder_signatures_with_profiles(&left_rows, &left_profiles);
    let right_signatures = build_folder_signatures_with_profiles(&right_rows, &right_profiles);

    let candidates = build_dossier_matches(&left_signatures, &right_signatures, args.top_k);
    let min_confidence = args.min_confidence;
    let mut candidates: Vec<DossierMatch> = candidates
        .into_iter()
        .filter(|item| item.confidence_tier.should_emit(min_confidence))
        .collect();
    if let Some(only_action) = args.only_action {
        candidates.retain(|item| item.confidence_tier.action() == only_action);
    }
    if args.one_per_left {
        candidates = keep_top_candidate_per_left(&candidates);
    }
    let mut confidence_counts = DossierConfidenceCounts::default();
    for item in &candidates {
        confidence_counts.bump(item.confidence_tier);
    }
    let archive_signal_candidates = candidates
        .iter()
        .filter(|item| item.shared_archive_family_count > 0)
        .count();
    let archive_signal_ratio = if candidates.is_empty() {
        0.0
    } else {
        (archive_signal_candidates as f64) / (candidates.len() as f64)
    };

    let report = DossierReport {
        report_schema: DOSSIER_REPORT_SCHEMA.to_string(),
        report_version: REPORT_VERSION_V1,
        left_db: args.left_db.display().to_string(),
        right_db: args.right_db.display().to_string(),
        left_label: args.left,
        right_label: args.right,
        top_k: args.top_k,
        min_confidence,
        only_action: args.only_action,
        left_folder_count: left_signatures.len(),
        right_folder_count: right_signatures.len(),
        archive_signal_candidates,
        archive_signal_ratio,
        cache_metrics: ReportCacheMetrics {
            left_profile_cache,
            right_profile_cache,
        },
        left_profile_cache,
        right_profile_cache,
        confidence_counts: confidence_counts.clone(),
        candidates: candidates.clone(),
    };

    let json = serde_json::to_string_pretty(&report)?;
    println!("{json}");

    if let Some(path) = args.out_json {
        std::fs::write(&path, format!("{json}\n"))
            .with_context(|| format!("failed to write {}", path.display()))?;
    }
    if let Some(path) = args.out_csv {
        let csv = build_dossier_csv(&candidates);
        std::fs::write(&path, csv)
            .with_context(|| format!("failed to write {}", path.display()))?;
    }
    if let Some(path) = args.out_actions_csv {
        let csv = build_dossier_actions_csv(&candidates);
        std::fs::write(&path, csv)
            .with_context(|| format!("failed to write {}", path.display()))?;
    }
    if let Some(best) = candidates.first() {
        eprintln!(
            "[dossier] summary: {} candidates across {} left folders and {} right folders; best {} -> {} ({:.3}, {} shared files, {})",
            candidates.len(),
            report.left_folder_count,
            report.right_folder_count,
            best.left_folder,
            best.right_folder,
            best.overlap_ratio,
            best.shared_rel_file_count,
            best.confidence_tier.as_str()
        );
        eprintln!(
            "[dossier] next action: {}",
            best.confidence_tier.next_action()
        );
        eprintln!(
            "[dossier] tiers: identical={} similar={} possible={} manual={}",
            confidence_counts.identical,
            confidence_counts.similar,
            confidence_counts.possible,
            confidence_counts.manual
        );
        eprintln!(
            "[dossier] archive-signal: candidates={} ratio={:.3}",
            archive_signal_candidates, archive_signal_ratio
        );
        eprintln!(
            "[dossier] profile-cache: left(hits={}, misses={}) right(hits={}, misses={})",
            left_profile_cache.hits,
            left_profile_cache.misses,
            right_profile_cache.hits,
            right_profile_cache.misses
        );
    } else {
        eprintln!(
            "[dossier] summary: 0 candidates across {} left folders and {} right folders",
            report.left_folder_count, report.right_folder_count
        );
        eprintln!(
            "[dossier] profile-cache: left(hits={}, misses={}) right(hits={}, misses={})",
            left_profile_cache.hits,
            left_profile_cache.misses,
            right_profile_cache.hits,
            right_profile_cache.misses
        );
    }
    Ok(())
}

fn logs_command(args: LogsArgs) -> Result<()> {
    let raw = std::fs::read_to_string(&args.file)
        .with_context(|| format!("failed to read {}", args.file.display()))?;
    let mut lines: Vec<&str> = raw.lines().collect();
    if args.tail > 0 && lines.len() > args.tail {
        lines = lines.split_off(lines.len() - args.tail);
    }

    let mut copied_bytes = 0u64;
    let mut planned_bytes = 0u64;
    let mut completed_files = 0u64;
    let mut planned_files = 0u64;
    let mut failed_files = 0u64;
    let mut failures = 0usize;
    let mut events_seen = 0usize;
    let mut error_classes: HashMap<String, usize> = HashMap::new();
    let mut retry_rows: Vec<serde_json::Value> = Vec::new();

    for line in &lines {
        let parsed: serde_json::Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(_) => continue,
        };

        let event = parsed
            .get("event")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if event.is_empty() {
            let action = parsed
                .get("action")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if action == "fail" {
                failures += 1;
                let reason = parsed
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                let class = reason
                    .split(':')
                    .next()
                    .unwrap_or(reason)
                    .trim()
                    .to_ascii_lowercase();
                *error_classes.entry(class).or_insert(0) += 1;
                retry_rows.push(serde_json::json!({
                    "rel_path": parsed.get("rel_path").and_then(|v| v.as_str()).unwrap_or_default(),
                    "reason": reason,
                    "bytes": parsed.get("bytes").and_then(|v| v.as_u64()).unwrap_or(0),
                }));
                if args.failures_only {
                    println!("{}", line);
                }
            }
            continue;
        }

        events_seen += 1;
        if event == "copy_progress" || event == "copy_summary" {
            copied_bytes = parsed
                .get("copied_bytes")
                .and_then(|v| v.as_u64())
                .unwrap_or(copied_bytes);
            planned_bytes = parsed
                .get("planned_bytes")
                .and_then(|v| v.as_u64())
                .unwrap_or(planned_bytes);
            completed_files = parsed
                .get("completed_files")
                .and_then(|v| v.as_u64())
                .or_else(|| parsed.get("copied_files").and_then(|v| v.as_u64()))
                .unwrap_or(completed_files);
            planned_files = parsed
                .get("planned_files")
                .and_then(|v| v.as_u64())
                .unwrap_or(planned_files);
            failed_files = parsed
                .get("failed_files")
                .and_then(|v| v.as_u64())
                .unwrap_or(failed_files);
        }
    }

    let pct = if planned_bytes > 0 {
        (copied_bytes as f64) * 100.0 / (planned_bytes as f64)
    } else {
        0.0
    };
    println!(
        "[logs] file={} events_seen={} copied={}/{} bytes ({:.2}%) files={}/{} failed={} failure_events={}",
        args.file.display(),
        events_seen,
        copied_bytes,
        planned_bytes,
        pct,
        completed_files,
        planned_files,
        failed_files,
        failures
    );
    if !error_classes.is_empty() {
        let mut ranked: Vec<(String, usize)> = error_classes.into_iter().collect();
        ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let topn = args.top_errors.min(ranked.len());
        for (class, count) in ranked.into_iter().take(topn) {
            println!("[logs errors] class={} count={}", class, count);
        }
    }
    if let Some(path) = args.retry_jsonl_out.as_ref() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(path)
            .with_context(|| format!("failed to open {}", path.display()))?;
        for row in &retry_rows {
            let payload = serde_json::to_vec(row)?;
            file.write_all(&payload)?;
            file.write_all(b"\n")?;
        }
        eprintln!("[logs] wrote retry candidates: {}", path.display());
    }

    Ok(())
}

#[derive(Debug, Serialize)]
struct StatusReport {
    report_schema: String,
    report_version: u32,
    labels: usize,
    resume_sessions: usize,
    recent_copy_runs: usize,
    signature_cache_rows: usize,
    virtual_archive_member_rows: usize,
    latest_bytes_per_second: Option<f64>,
}

#[derive(Debug, Serialize)]
struct InspectCacheLabelRow {
    label: String,
    files: usize,
    profiles: usize,
    profile_coverage_ratio: f64,
    with_binary_signature: usize,
    with_binary_descriptor: usize,
    with_text_signature: usize,
    with_archive_signature: usize,
}

#[derive(Debug, Serialize)]
struct InspectCacheReport {
    report_schema: String,
    report_version: u32,
    signature_cache_rows: usize,
    virtual_archive_member_rows: usize,
    labels: Vec<InspectCacheLabelRow>,
}

fn status_command(args: StatusArgs) -> Result<()> {
    let conn = open_db(&args.db)?;
    let labels: i64 = conn.query_row("SELECT COUNT(DISTINCT label) FROM files", [], |row| {
        row.get(0)
    })?;
    let sessions: i64 = conn.query_row("SELECT COUNT(*) FROM copy_resume_sessions", [], |row| {
        row.get(0)
    })?;
    let now = now_ns()?;
    let since = now - args.window_minutes.max(1) * 60 * 1_000_000_000;
    let recent_runs: i64 = conn.query_row(
        "SELECT COUNT(*) FROM copy_runs WHERE copied_at >= ?1",
        params![since],
        |row| row.get(0),
    )?;
    let signature_cache_rows: i64 =
        conn.query_row("SELECT COUNT(*) FROM signature_cache", [], |row| row.get(0))?;
    let virtual_archive_member_rows: i64 =
        conn.query_row("SELECT COUNT(*) FROM virtual_archive_members", [], |row| {
            row.get(0)
        })?;
    let latest_rate: Option<f64> = conn
        .query_row(
            "SELECT copied_bytes, duration_ns FROM copy_runs WHERE copied_bytes > 0 AND duration_ns > 0 ORDER BY copied_at DESC LIMIT 1",
            [],
            |row| {
                let bytes: i64 = row.get(0)?;
                let dur: i64 = row.get(1)?;
                if bytes <= 0 || dur <= 0 {
                    Ok(None)
                } else {
                    Ok(Some((bytes as f64) / (dur as f64 / 1_000_000_000.0)))
                }
            },
        )
        .optional()?
        .flatten();
    let report = StatusReport {
        report_schema: "nightindex.status".to_string(),
        report_version: 1,
        labels: labels.max(0) as usize,
        resume_sessions: sessions.max(0) as usize,
        recent_copy_runs: recent_runs.max(0) as usize,
        signature_cache_rows: signature_cache_rows.max(0) as usize,
        virtual_archive_member_rows: virtual_archive_member_rows.max(0) as usize,
        latest_bytes_per_second: latest_rate,
    };
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

fn inspect_cache_command(args: InspectCacheArgs) -> Result<()> {
    let conn = open_db(&args.db)?;
    let signature_cache_rows: i64 =
        conn.query_row("SELECT COUNT(*) FROM signature_cache", [], |row| row.get(0))?;
    let virtual_archive_member_rows: i64 =
        conn.query_row("SELECT COUNT(*) FROM virtual_archive_members", [], |row| {
            row.get(0)
        })?;

    let labels = if let Some(label) = args.label.as_deref() {
        vec![label.to_string()]
    } else {
        let mut stmt = conn.prepare("SELECT DISTINCT label FROM files ORDER BY label")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        out
    };

    let mut label_rows = Vec::new();
    for label in labels {
        let files: i64 = conn.query_row(
            "SELECT COUNT(*) FROM files WHERE label = ?1",
            params![&label],
            |row| row.get(0),
        )?;
        let (profiles, with_binary_signature, with_binary_descriptor, with_text_signature, with_archive_signature): (i64, i64, i64, i64, i64) =
            conn.query_row(
                "SELECT
                    COUNT(*),
                    SUM(CASE WHEN binary_signature IS NOT NULL AND binary_signature <> '' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN binary_descriptor IS NOT NULL AND binary_descriptor <> '' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN text_signature IS NOT NULL AND text_signature <> '' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN archive_signature IS NOT NULL AND archive_signature <> '' THEN 1 ELSE 0 END)
                 FROM file_fingerprints
                 WHERE label = ?1",
                params![&label],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Option<i64>>(1)?.unwrap_or(0),
                        row.get::<_, Option<i64>>(2)?.unwrap_or(0),
                        row.get::<_, Option<i64>>(3)?.unwrap_or(0),
                        row.get::<_, Option<i64>>(4)?.unwrap_or(0),
                    ))
                },
            )?;

        let files_usize = files.max(0) as usize;
        let profiles_usize = profiles.max(0) as usize;
        let profile_coverage_ratio = if files_usize == 0 {
            0.0
        } else {
            (profiles_usize as f64) / (files_usize as f64)
        };
        label_rows.push(InspectCacheLabelRow {
            label,
            files: files_usize,
            profiles: profiles_usize,
            profile_coverage_ratio,
            with_binary_signature: with_binary_signature.max(0) as usize,
            with_binary_descriptor: with_binary_descriptor.max(0) as usize,
            with_text_signature: with_text_signature.max(0) as usize,
            with_archive_signature: with_archive_signature.max(0) as usize,
        });
    }

    let report = InspectCacheReport {
        report_schema: "nightindex.inspect_cache".to_string(),
        report_version: 1,
        signature_cache_rows: signature_cache_rows.max(0) as usize,
        virtual_archive_member_rows: virtual_archive_member_rows.max(0) as usize,
        labels: label_rows,
    };
    let json = serde_json::to_string_pretty(&report)?;
    println!("{json}");
    if let Some(path) = args.out_json {
        std::fs::write(&path, format!("{json}\n"))
            .with_context(|| format!("failed to write {}", path.display()))?;
    }
    Ok(())
}

fn parse_simple_csv_line(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let bytes = line.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let ch = bytes[i] as char;
        if ch == '"' {
            if in_quotes && i + 1 < bytes.len() && bytes[i + 1] as char == '"' {
                cur.push('"');
                i += 2;
                continue;
            }
            in_quotes = !in_quotes;
            i += 1;
            continue;
        }
        if ch == ',' && !in_quotes {
            out.push(cur.clone());
            cur.clear();
            i += 1;
            continue;
        }
        cur.push(ch);
        i += 1;
    }
    out.push(cur);
    out
}

fn parse_merge_actions_csv(path: &Path) -> Result<Vec<MergeActionCsvRow>> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let mut rows = Vec::new();
    for (idx, line) in raw.lines().enumerate() {
        if idx == 0 || line.trim().is_empty() {
            continue;
        }
        let cols = parse_simple_csv_line(line);
        if cols.len() < 9 {
            continue;
        }
        rows.push(MergeActionCsvRow {
            left_folder: cols[0].clone(),
            rank: cols[1].parse().unwrap_or(0),
            right_folder: cols[2].clone(),
            confidence_tier: cols[3].clone(),
            next_action: cols[4].clone(),
            overlap_ratio: cols[5].parse().unwrap_or(0.0),
            shared_hash_count: cols[6].parse().unwrap_or(0),
            shared_normalized_file_name_count: cols[7].parse().unwrap_or(0),
            shared_rel_file_count: cols[8].parse().unwrap_or(0),
        });
    }
    Ok(rows)
}

fn merge_decision_for(row: &MergeActionCsvRow, policy: MergePolicy) -> (&'static str, String) {
    if row.next_action.contains("manual") {
        return ("manual", "dossier suggested manual review".to_string());
    }
    match policy {
        MergePolicy::PreferNewer => ("apply", "prefer-newer policy".to_string()),
        MergePolicy::PreferLarger => ("apply", "prefer-larger policy".to_string()),
        MergePolicy::KeepBoth => ("keep_both", "keep-both policy".to_string()),
        MergePolicy::Manual => ("manual", "manual policy".to_string()),
    }
}

fn merge_plan_command(args: MergePlanArgs) -> Result<()> {
    let rows = parse_merge_actions_csv(&args.actions_csv)?;
    let mut items = Vec::new();
    for row in rows {
        if row.rank != 1 {
            continue;
        }
        let source = args.imports_root.join(&row.left_folder);
        let destination = args.canonical_root.join(&row.right_folder);
        let (decision, reason) = merge_decision_for(&row, args.policy);
        items.push(MergePlanItem {
            left_folder: row.left_folder,
            right_folder: row.right_folder,
            source: source.display().to_string(),
            destination: destination.display().to_string(),
            decision: decision.to_string(),
            reason,
        });
    }
    items.sort_by(|a, b| {
        a.left_folder
            .cmp(&b.left_folder)
            .then(a.right_folder.cmp(&b.right_folder))
    });
    let plan = MergePlan {
        schema_version: 1,
        generated_at_ns: now_ns()?,
        policy: args.policy,
        imports_root: args.imports_root.display().to_string(),
        canonical_root: args.canonical_root.display().to_string(),
        items,
    };
    if let Some(parent) = args.out_json.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(&plan)?;
    std::fs::write(&args.out_json, format!("{json}\n"))
        .with_context(|| format!("failed to write {}", args.out_json.display()))?;
    println!("{json}");
    Ok(())
}

fn merge_apply_command(args: MergeApplyArgs) -> Result<()> {
    let raw = std::fs::read_to_string(&args.plan)
        .with_context(|| format!("failed to read {}", args.plan.display()))?;
    let plan: MergePlan = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse {}", args.plan.display()))?;
    let mut applied = 0usize;
    let mut manual = 0usize;
    let mut kept_both = 0usize;
    let mut skipped_existing = 0usize;
    let mut conflicts = 0usize;
    let mut failed = 0usize;
    let mut renamed_targets = 0usize;
    for item in &plan.items {
        match item.decision.as_str() {
            "manual" => {
                manual += 1;
                continue;
            }
            "keep_both" => {
                kept_both += 1;
                let source = Path::new(&item.source);
                let destination = Path::new(&item.destination);
                if !source.exists() {
                    failed += 1;
                    continue;
                }
                if source.is_file() {
                    let target = unique_keep_both_path(destination);
                    renamed_targets += 1;
                    if args.dry_run {
                        applied += 1;
                        continue;
                    }
                    if let Some(parent) = target.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    fs::copy(source, &target)?;
                    applied += 1;
                } else if source.is_dir() {
                    let target = unique_keep_both_path(destination);
                    renamed_targets += 1;
                    if args.dry_run {
                        applied += 1;
                        continue;
                    }
                    copy_directory_tree(
                        source,
                        &target,
                        false,
                        &mut skipped_existing,
                        &mut conflicts,
                    )?;
                    applied += 1;
                } else {
                    failed += 1;
                }
            }
            _ => {
                let source = Path::new(&item.source);
                let dest = Path::new(&item.destination);
                if !source.exists() {
                    failed += 1;
                    continue;
                }
                if source.is_file() {
                    if destination_entry_same(source, dest)? {
                        skipped_existing += 1;
                        continue;
                    }
                    if args.dry_run {
                        applied += 1;
                        continue;
                    }
                    if let Some(parent) = dest.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    if dest.exists() {
                        remove_destination_entry(dest)?;
                    }
                    fs::copy(source, dest)?;
                    applied += 1;
                } else if source.is_dir() {
                    if args.dry_run {
                        applied += 1;
                        continue;
                    }
                    copy_directory_tree(source, dest, true, &mut skipped_existing, &mut conflicts)?;
                    applied += 1;
                } else {
                    failed += 1;
                }
            }
        }
    }
    println!(
        "{{\"applied\":{},\"manual\":{},\"kept_both\":{},\"skipped_existing\":{},\"conflicts\":{},\"failed\":{},\"renamed_targets\":{},\"dry_run\":{},\"policy\":\"{}\"}}",
        applied,
        manual,
        kept_both,
        skipped_existing,
        conflicts,
        failed,
        renamed_targets,
        args.dry_run,
        plan.policy
    );
    Ok(())
}

fn unique_keep_both_path(base: &Path) -> PathBuf {
    let mut candidate = PathBuf::from(format!("{}.from_import", base.display()));
    if !candidate.exists() {
        return candidate;
    }
    for idx in 1..=9_999usize {
        candidate = PathBuf::from(format!("{}.from_import.{idx}", base.display()));
        if !candidate.exists() {
            return candidate;
        }
    }
    PathBuf::from(format!("{}.from_import.fallback", base.display()))
}

fn destination_entry_same(source: &Path, destination: &Path) -> Result<bool> {
    if !destination.exists() {
        return Ok(false);
    }
    let source_meta = fs::symlink_metadata(source)
        .with_context(|| format!("failed to stat {}", source.display()))?;
    let dest_meta = fs::symlink_metadata(destination)
        .with_context(|| format!("failed to stat {}", destination.display()))?;
    if source_meta.is_file() && dest_meta.is_file() {
        if source_meta.len() != dest_meta.len() {
            return Ok(false);
        }
        let source_hash = blake3_file(source)?;
        let dest_hash = blake3_file(destination)?;
        return Ok(source_hash == dest_hash);
    }
    Ok(false)
}

fn remove_destination_entry(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("failed to stat {}", path.display()))?;
    if metadata.file_type().is_symlink() || metadata.is_file() {
        fs::remove_file(path).with_context(|| format!("failed to remove {}", path.display()))?;
    } else if metadata.is_dir() {
        fs::remove_dir_all(path).with_context(|| format!("failed to remove {}", path.display()))?;
    } else {
        fs::remove_file(path).with_context(|| format!("failed to remove {}", path.display()))?;
    }
    Ok(())
}

fn copy_directory_tree(
    source_root: &Path,
    destination_root: &Path,
    overwrite: bool,
    skipped_existing: &mut usize,
    conflicts: &mut usize,
) -> Result<()> {
    fs::create_dir_all(destination_root)
        .with_context(|| format!("failed to create {}", destination_root.display()))?;
    for entry in WalkDir::new(source_root).into_iter().filter_map(Result::ok) {
        let path = entry.path();
        let rel = match path.strip_prefix(source_root) {
            Ok(value) => value,
            Err(_) => continue,
        };
        if rel.as_os_str().is_empty() {
            continue;
        }
        let destination = destination_root.join(rel);
        let file_type = entry.file_type();
        if file_type.is_dir() {
            fs::create_dir_all(&destination)
                .with_context(|| format!("failed to create {}", destination.display()))?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        if destination_entry_same(path, &destination)? {
            *skipped_existing += 1;
            continue;
        }
        if destination.exists() && !overwrite {
            *conflicts += 1;
            continue;
        }
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        if destination.exists() {
            remove_destination_entry(&destination)?;
        }
        fs::copy(path, &destination).with_context(|| {
            format!(
                "failed to copy {} -> {}",
                path.display(),
                destination.display()
            )
        })?;
    }
    Ok(())
}

#[derive(Debug)]
struct CopyRunSample {
    bytes_copied: u64,
    duration_ns: i64,
    copied_at: i64,
}

fn build_compare_and_copy_summary(
    left_rows: &[FileRecord],
    right_rows: &[FileRecord],
    left: &str,
    right: &str,
    cache_metrics: ReportCacheMetrics,
) -> Result<(CompareSummary, usize, u64)> {
    let mut left_map: HashMap<String, FileRecord> = HashMap::with_capacity(left_rows.len());
    let mut right_map: HashMap<String, FileRecord> = HashMap::with_capacity(right_rows.len());

    for row in left_rows {
        left_map.insert(row.rel_path.clone(), row.clone());
    }
    for row in right_rows {
        right_map.insert(row.rel_path.clone(), row.clone());
    }

    let mut same_path_same_meta = 0usize;
    let mut same_path_changed = 0usize;
    let mut left_only = 0usize;
    let mut files_to_copy = 0usize;
    let mut bytes_to_copy = 0u64;

    for (rel_path, left_row) in &left_map {
        match right_map.get(rel_path) {
            Some(right_row) => {
                if left_row.size == right_row.size
                    && left_row.mtime_ns == right_row.mtime_ns
                    && left_row.fast_hash == right_row.fast_hash
                {
                    same_path_same_meta += 1;
                } else {
                    same_path_changed += 1;
                }
            }
            None => {
                left_only += 1;
                files_to_copy += 1;
                bytes_to_copy += left_row.size;
            }
        }
    }

    let right_only = right_map
        .keys()
        .filter(|rel_path| !left_map.contains_key(*rel_path))
        .count();

    let summary = CompareSummary {
        report_schema: COMPARE_SUMMARY_REPORT_SCHEMA.to_string(),
        report_version: REPORT_VERSION_V1,
        left_label: left.to_string(),
        right_label: right.to_string(),
        left_files: left_map.len(),
        right_files: right_map.len(),
        same_path_same_meta,
        same_path_changed,
        left_only,
        right_only,
        cache_metrics,
    };

    Ok((summary, files_to_copy, bytes_to_copy))
}

fn build_compare_summary_csv(summary: &CompareSummary) -> String {
    let mut csv = String::new();
    csv.push_str(
        "left_label,right_label,left_files,right_files,same_path_same_meta,same_path_changed,left_only,right_only\n",
    );
    let _ = std::fmt::Write::write_fmt(
        &mut csv,
        format_args!(
            "{},{},{},{},{},{},{},{}\n",
            csv_escape(&summary.left_label),
            csv_escape(&summary.right_label),
            summary.left_files,
            summary.right_files,
            summary.same_path_same_meta,
            summary.same_path_changed,
            summary.left_only,
            summary.right_only
        ),
    );
    csv
}

fn build_brief_csv(summary: &BriefSummary) -> String {
    let mut csv = String::new();
    csv.push_str(
        "left_label,right_label,left_files,right_files,same_path_same_meta,same_path_changed,left_only,right_only,files_to_copy,bytes_to_copy,prior_bytes_per_second,estimated_seconds\n",
    );
    let prior = summary
        .prior_bytes_per_second
        .map_or_else(|| "".to_string(), |value| format!("{value:.6}"));
    let estimated = summary
        .estimated_seconds
        .map_or_else(|| "".to_string(), |value| value.to_string());
    let _ = std::fmt::Write::write_fmt(
        &mut csv,
        format_args!(
            "{},{},{},{},{},{},{},{},{},{},{},{}\n",
            csv_escape(&summary.left_label),
            csv_escape(&summary.right_label),
            summary.left_files,
            summary.right_files,
            summary.same_path_same_meta,
            summary.same_path_changed,
            summary.left_only,
            summary.right_only,
            summary.files_to_copy,
            summary.bytes_to_copy,
            prior,
            estimated
        ),
    );
    csv
}

fn latest_copy_run_sample(
    conn: &Connection,
    left_label: &str,
    right_label: &str,
) -> Result<Option<CopyRunSample>> {
    let query = "
        SELECT copied_bytes, duration_ns, copied_at
        FROM copy_runs
        WHERE left_label = ?1 AND right_label = ?2 AND mode = 'copy-missing' AND copied_bytes > 0 AND duration_ns > 0
        ORDER BY copied_at DESC
        LIMIT 1
    ";
    let sample = conn
        .query_row(query, params![left_label, right_label], |row| {
            Ok(CopyRunSample {
                bytes_copied: row.get(0)?,
                duration_ns: row.get(1)?,
                copied_at: row.get(2)?,
            })
        })
        .optional()?;
    Ok(sample)
}

fn estimate_copy_eta(
    sample: Option<&CopyRunSample>,
    bytes_to_copy: u64,
) -> (Option<f64>, Option<u64>) {
    let Some(sample) = sample else {
        return (None, None);
    };

    if sample.bytes_copied == 0 || sample.duration_ns <= 0 {
        return (None, None);
    }

    let duration_seconds = (sample.duration_ns as f64) / 1_000_000_000.0;
    if duration_seconds <= 0.0 {
        return (None, None);
    }

    let bytes_per_second = (sample.bytes_copied as f64) / duration_seconds;
    if bytes_per_second <= 0.0 {
        return (None, None);
    }

    let estimated_seconds = ((bytes_to_copy as f64) / bytes_per_second).round() as u64;
    (Some(bytes_per_second), Some(estimated_seconds))
}

fn record_copy_run_stats(plan: &CopyPlan, summary: &CopyExecutionSummary, elapsed_ns: i64) {
    let mut write_to_paths = Vec::new();
    if let Some(left_db) = &plan.left_db {
        write_to_paths.push(left_db.as_str());
    }
    if let Some(right_db) = &plan.right_db {
        if Some(right_db.as_str()) != plan.left_db.as_deref() {
            write_to_paths.push(right_db.as_str());
        }
    }

    for db_path in write_to_paths {
        let conn = match open_db(Path::new(db_path)) {
            Ok(conn) => conn,
            Err(err) => {
                eprintln!("[warn] failed to open copy stat db {db_path}: {err}");
                continue;
            }
        };

        if let Err(err) = conn.execute(
            "INSERT INTO copy_runs(left_label, right_label, mode, planned_files, copied_files, bytes_to_copy, copied_bytes, duration_ns, copied_at) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                &plan.left_label,
                &plan.right_label,
                &plan.mode,
                summary.planned_files as i64,
                summary.copied_files as i64,
                plan.summary.bytes_to_copy as i64,
                summary.copied_bytes as i64,
                elapsed_ns,
                now_ns().unwrap_or_default(),
            ],
        ) {
            eprintln!("[warn] failed to write copy run stats to {db_path}: {err}");
        }
    }
}

struct ResumeRecorder {
    session_id: String,
    db_paths: Vec<String>,
}

impl ResumeRecorder {
    fn start(plan: &CopyPlan, args: &CopyRunArgs, planned_files: usize) -> Result<Option<Self>> {
        let mut db_paths = Vec::new();
        if let Some(left_db) = &plan.left_db {
            db_paths.push(left_db.clone());
        }
        if let Some(right_db) = &plan.right_db {
            if Some(right_db.as_str()) != plan.left_db.as_deref() {
                db_paths.push(right_db.clone());
            }
        }
        if db_paths.is_empty() {
            return Ok(None);
        }

        let started_at = now_ns()?;
        let session_id = format!("{}-{}", std::process::id(), started_at);
        for db_path in &db_paths {
            let conn = open_db(Path::new(db_path))?;
            conn.execute(
                "INSERT INTO copy_resume_sessions(session_id, mode, left_label, right_label, source_root, destination_root, started_at, planned_files) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    &session_id,
                    &plan.mode,
                    &plan.left_label,
                    &plan.right_label,
                    args.source_root.display().to_string(),
                    args.destination_root.display().to_string(),
                    started_at,
                    planned_files as i64
                ],
            )?;
        }
        Ok(Some(Self {
            session_id,
            db_paths,
        }))
    }

    fn mark_status(
        &self,
        rel_path: &str,
        status: &str,
        bytes_done: u64,
        error: Option<&str>,
        increment_attempt: bool,
    ) -> Result<()> {
        let updated_at = now_ns()?;
        for db_path in &self.db_paths {
            let conn = open_db(Path::new(db_path))?;
            let attempts_expr = if increment_attempt {
                "attempts + 1"
            } else {
                "attempts"
            };
            let sql = format!(
                "INSERT INTO copy_resume_items(session_id, rel_path, status, attempts, bytes_done, last_error, updated_at) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(session_id, rel_path) DO UPDATE SET
                    status = excluded.status,
                    attempts = {attempts_expr},
                    bytes_done = excluded.bytes_done,
                    last_error = excluded.last_error,
                    updated_at = excluded.updated_at"
            );
            conn.execute(
                &sql,
                params![
                    &self.session_id,
                    rel_path,
                    status,
                    if increment_attempt { 1i64 } else { 0i64 },
                    bytes_done as i64,
                    error,
                    updated_at
                ],
            )?;
        }
        Ok(())
    }

    fn finish(&self, copied_files: usize, failed_files: usize) -> Result<()> {
        let finished_at = now_ns()?;
        for db_path in &self.db_paths {
            let conn = open_db(Path::new(db_path))?;
            conn.execute(
                "UPDATE copy_resume_sessions SET finished_at = ?2, copied_files = ?3, failed_files = ?4 WHERE session_id = ?1",
                params![&self.session_id, finished_at, copied_files as i64, failed_files as i64],
            )?;
        }
        Ok(())
    }
}

fn load_resume_pending_items(
    conn: &Connection,
    session_id: &str,
    only_failed: bool,
    max_attempts: Option<u64>,
) -> Result<Vec<String>> {
    let status_sql = if only_failed {
        "status = 'failed'"
    } else {
        "status IN ('pending', 'copying', 'failed')"
    };
    let mut sql = format!(
        "SELECT rel_path FROM copy_resume_items WHERE session_id = ?1 AND {}",
        status_sql
    );
    if let Some(max_attempts) = max_attempts {
        sql.push_str(&format!(" AND attempts <= {}", max_attempts));
    }
    sql.push_str(" ORDER BY rel_path");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![session_id], |row| row.get::<_, String>(0))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

#[derive(Debug)]
struct ResumeSessionMeta {
    session_id: String,
    mode: String,
    left_label: String,
    right_label: String,
}

#[derive(Debug, Serialize)]
struct ResumeSessionListItem {
    session_id: String,
    mode: String,
    left_label: String,
    right_label: String,
    started_at: i64,
    finished_at: Option<i64>,
    planned_files: usize,
    copied_files: usize,
    failed_files: usize,
}

#[derive(Debug, Serialize)]
struct ResumeSessionStats {
    session_id: String,
    pending: usize,
    copying: usize,
    failed: usize,
    done: usize,
    skipped_existing: usize,
    skipped_conflict: usize,
    planned: usize,
}

#[derive(Debug, Serialize)]
struct ResumePruneResult {
    scope: String,
    deleted_rows: usize,
    dry_run: bool,
}

#[derive(Debug, Serialize)]
struct ResumeItemExportRow {
    session_id: String,
    rel_path: String,
    status: String,
    attempts: usize,
    bytes_done: u64,
    last_error: Option<String>,
    updated_at: i64,
}

fn load_resume_session_meta(
    conn: &Connection,
    session_id: Option<&str>,
) -> Result<Option<ResumeSessionMeta>> {
    if let Some(session_id) = session_id {
        return conn
            .query_row(
                "SELECT session_id, mode, left_label, right_label FROM copy_resume_sessions WHERE session_id = ?1",
                params![session_id],
                |row| {
                    Ok(ResumeSessionMeta {
                        session_id: row.get(0)?,
                        mode: row.get(1)?,
                        left_label: row.get(2)?,
                        right_label: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into);
    }

    conn.query_row(
        "SELECT session_id, mode, left_label, right_label FROM copy_resume_sessions ORDER BY started_at DESC LIMIT 1",
        [],
        |row| {
            Ok(ResumeSessionMeta {
                session_id: row.get(0)?,
                mode: row.get(1)?,
                left_label: row.get(2)?,
                right_label: row.get(3)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

fn list_resume_sessions(conn: &Connection) -> Result<Vec<ResumeSessionListItem>> {
    let mut stmt = conn.prepare(
        "SELECT session_id, mode, left_label, right_label, started_at, finished_at, planned_files, copied_files, failed_files
         FROM copy_resume_sessions ORDER BY started_at DESC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(ResumeSessionListItem {
            session_id: row.get(0)?,
            mode: row.get(1)?,
            left_label: row.get(2)?,
            right_label: row.get(3)?,
            started_at: row.get(4)?,
            finished_at: row.get(5)?,
            planned_files: row.get::<_, i64>(6)?.max(0) as usize,
            copied_files: row.get::<_, i64>(7)?.max(0) as usize,
            failed_files: row.get::<_, i64>(8)?.max(0) as usize,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

fn load_resume_session_stats(conn: &Connection, session_id: &str) -> Result<ResumeSessionStats> {
    let mut stats = ResumeSessionStats {
        session_id: session_id.to_string(),
        pending: 0,
        copying: 0,
        failed: 0,
        done: 0,
        skipped_existing: 0,
        skipped_conflict: 0,
        planned: 0,
    };
    let mut stmt = conn.prepare(
        "SELECT status, COUNT(*) FROM copy_resume_items WHERE session_id = ?1 GROUP BY status",
    )?;
    let rows = stmt.query_map(params![session_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    for row in rows {
        let (status, count) = row?;
        let count = count.max(0) as usize;
        match status.as_str() {
            "pending" => stats.pending = count,
            "copying" => stats.copying = count,
            "failed" => stats.failed = count,
            "done" => stats.done = count,
            "skipped_existing" => stats.skipped_existing = count,
            "skipped_conflict" => stats.skipped_conflict = count,
            "planned" => stats.planned = count,
            _ => {}
        }
    }
    Ok(stats)
}

fn load_resume_items_for_export(
    conn: &Connection,
    session_id: &str,
    only_failed: bool,
    max_attempts: Option<u64>,
) -> Result<Vec<ResumeItemExportRow>> {
    let status_sql = if only_failed {
        "status = 'failed'"
    } else {
        "status IN ('pending', 'copying', 'failed', 'done', 'skipped_existing', 'skipped_conflict', 'planned')"
    };
    let mut sql = format!(
        "SELECT rel_path, status, attempts, bytes_done, last_error, updated_at
         FROM copy_resume_items
         WHERE session_id = ?1 AND {}",
        status_sql
    );
    if let Some(max_attempts) = max_attempts {
        sql.push_str(&format!(" AND attempts <= {}", max_attempts));
    }
    sql.push_str(" ORDER BY rel_path");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![session_id], |row| {
        Ok(ResumeItemExportRow {
            session_id: session_id.to_string(),
            rel_path: row.get(0)?,
            status: row.get(1)?,
            attempts: row.get::<_, i64>(2)?.max(0) as usize,
            bytes_done: row.get::<_, i64>(3)?.max(0) as u64,
            last_error: row.get(4)?,
            updated_at: row.get(5)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

fn prune_resume_completed_rows(
    conn: &Connection,
    session_id: Option<&str>,
    dry_run: bool,
) -> Result<ResumePruneResult> {
    let where_clause = if session_id.is_some() {
        "session_id = ?1 AND status IN ('done', 'skipped_existing', 'skipped_conflict')"
    } else {
        "status IN ('done', 'skipped_existing', 'skipped_conflict')"
    };
    let (scope, deleted_rows) = if let Some(session_id) = session_id {
        if dry_run {
            let query = format!(
                "SELECT COUNT(*) FROM copy_resume_items WHERE {}",
                where_clause
            );
            let count: i64 = conn.query_row(&query, params![session_id], |row| row.get(0))?;
            (format!("session:{session_id}"), count.max(0) as usize)
        } else {
            let query = format!("DELETE FROM copy_resume_items WHERE {}", where_clause);
            let changed = conn.execute(&query, params![session_id])?;
            (format!("session:{session_id}"), changed)
        }
    } else if dry_run {
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM copy_resume_items WHERE status IN ('done', 'skipped_existing', 'skipped_conflict')",
            [],
            |row| row.get(0),
        )?;
        ("all".to_string(), count.max(0) as usize)
    } else {
        let changed = conn.execute(
            "DELETE FROM copy_resume_items WHERE status IN ('done', 'skipped_existing', 'skipped_conflict')",
            [],
        )?;
        ("all".to_string(), changed)
    };
    Ok(ResumePruneResult {
        scope,
        deleted_rows,
        dry_run,
    })
}

fn build_resume_copy_plan(
    db: &Path,
    session_id: Option<&str>,
    only_failed: bool,
    max_attempts: Option<u64>,
) -> Result<CopyPlan> {
    let conn = open_db(db)?;
    let Some(meta) = load_resume_session_meta(&conn, session_id)? else {
        bail!("no resume session found");
    };
    let pending = load_resume_pending_items(&conn, &meta.session_id, only_failed, max_attempts)?;
    let mut items = Vec::new();
    let mut bytes_to_copy = 0u64;
    for rel_path in pending {
        let from_files = conn
            .query_row(
                "SELECT file_type, size, mtime_ns, fast_hash FROM files WHERE label = ?1 AND rel_path = ?2",
                params![&meta.left_label, &rel_path],
                |row| {
                    Ok(CopyPlanItem {
                        rel_path: rel_path.clone(),
                        file_type: row.get(0)?,
                        size: row.get::<_, u64>(1)?,
                        mtime_ns: row.get::<_, i64>(2)?,
                        fast_hash: row.get(3)?,
                    })
                },
            )
            .optional()?;
        let item = from_files.unwrap_or_else(|| CopyPlanItem {
            rel_path: rel_path.clone(),
            file_type: "file".to_string(),
            size: 0,
            mtime_ns: 0,
            fast_hash: None,
        });
        bytes_to_copy = bytes_to_copy.saturating_add(item.size);
        items.push(item);
    }

    Ok(CopyPlan {
        mode: meta.mode,
        left_label: meta.left_label,
        right_label: meta.right_label,
        left_db: Some(db.display().to_string()),
        right_db: None,
        generated_at_ns: now_ns()?,
        summary: CopyPlanSummary {
            files_to_copy: items.len(),
            bytes_to_copy,
            left_files: items.len(),
            right_files: 0,
        },
        items,
    })
}

fn build_dossier_matches(
    left_signatures: &HashMap<String, FolderSignature>,
    right_signatures: &HashMap<String, FolderSignature>,
    top_k: usize,
) -> Vec<DossierMatch> {
    let right_index = build_folder_token_index(right_signatures);
    let mut output = Vec::new();

    for (left_folder, left_signature) in left_signatures {
        if left_signature.tokens.is_empty() {
            continue;
        }

        let mut match_states: HashMap<String, DossierMatchState> = HashMap::new();

        for (token, left_weight) in &left_signature.tokens {
            let Some(candidates) = right_index.get(token.as_str()) else {
                continue;
            };
            for (right_folder, right_weight) in candidates {
                let state = match_states.entry(right_folder.clone()).or_default();
                let shared = left_weight.min(*right_weight);
                state.shared_weight += shared;
                match dossier_token_family(token) {
                    DossierTokenFamily::ExactFileName => {
                        state.shared_rel_file_count += 1;
                        state.shared_file_name_weight += shared;
                        state.shared_file_name_count += 1;
                    }
                    DossierTokenFamily::NormalizedFileName => {
                        state.shared_normalized_file_name_weight += shared;
                        state.shared_normalized_file_name_count += 1;
                    }
                    DossierTokenFamily::FileStem => {
                        state.shared_file_stem_weight += shared;
                        state.shared_file_stem_count += 1;
                    }
                    DossierTokenFamily::FileExtension => {
                        state.shared_file_ext_weight += shared;
                        state.shared_file_ext_count += 1;
                    }
                    DossierTokenFamily::ExtensionStem => {
                        state.shared_ext_stem_weight += shared;
                        state.shared_ext_stem_count += 1;
                    }
                    DossierTokenFamily::Hash => {
                        state.shared_hash_weight += shared;
                        state.shared_hash_count += 1;
                    }
                    DossierTokenFamily::Binaryity => {
                        state.shared_binaryity_weight += shared;
                        state.shared_binaryity_count += 1;
                    }
                    DossierTokenFamily::ArchiveFamily => {
                        state.shared_archive_family_weight += shared;
                        state.shared_archive_family_count += 1;
                    }
                    DossierTokenFamily::ArchiveSignature => {
                        state.shared_archive_signature_weight += shared;
                        state.shared_archive_signature_count += 1;
                    }
                    DossierTokenFamily::Language => {
                        state.shared_language_weight += shared;
                        state.shared_language_count += 1;
                    }
                    DossierTokenFamily::SizeClass => {
                        state.shared_size_class_weight += shared;
                        state.shared_size_class_count += 1;
                    }
                    DossierTokenFamily::NormalizedFolder => {
                        state.shared_normalized_parent_folder_weight += shared;
                        state.shared_normalized_parent_folder_count += 1;
                    }
                    DossierTokenFamily::Folder => {
                        state.shared_folder_weight += shared;
                        state.shared_folder_count += 1;
                    }
                    DossierTokenFamily::Other => {}
                }
            }
        }

        let mut ranked: Vec<(DossierMatch, DossierMatchState)> = Vec::new();
        for (right_folder, state) in match_states {
            let right_signature = match right_signatures.get(&right_folder) {
                Some(signature) => signature,
                None => continue,
            };

            let denominator =
                left_signature.total_weight + right_signature.total_weight - state.shared_weight;
            if !(denominator > 0.0) || state.shared_weight == 0.0 {
                continue;
            }

            let overlap_ratio = state.shared_weight / denominator;
            let dossier_match = DossierMatch {
                left_folder: left_folder.clone(),
                right_folder,
                overlap_weight: state.shared_weight,
                left_weight: left_signature.total_weight,
                right_weight: right_signature.total_weight,
                overlap_ratio,
                shared_rel_file_count: state.shared_rel_file_count,
                shared_exact_file_name_count: state.shared_file_name_count,
                shared_normalized_file_name_count: state.shared_normalized_file_name_count,
                shared_file_stem_count: state.shared_file_stem_count,
                shared_file_ext_count: state.shared_file_ext_count,
                shared_ext_stem_count: state.shared_ext_stem_count,
                shared_hash_count: state.shared_hash_count,
                shared_folder_token_count: state.shared_folder_count,
                shared_normalized_parent_folder_count: state.shared_normalized_parent_folder_count,
                shared_binaryity_count: state.shared_binaryity_count,
                shared_archive_family_count: state.shared_archive_family_count,
                shared_language_count: state.shared_language_count,
                shared_size_class_count: state.shared_size_class_count,
                confidence_tier: DossierConfidenceTier::Manual,
            };
            let dossier_match = DossierMatch {
                confidence_tier: dossier_confidence_tier(&dossier_match),
                ..dossier_match
            };
            ranked.push((dossier_match, state));
        }

        ranked.sort_by(|a, b| {
            let (a_match, a_state) = a;
            let (b_match, b_state) = b;
            b_match
                .overlap_ratio
                .partial_cmp(&a_match.overlap_ratio)
                .unwrap_or(Ordering::Equal)
                .then_with(|| {
                    b_match
                        .overlap_weight
                        .partial_cmp(&a_match.overlap_weight)
                        .unwrap_or(Ordering::Equal)
                })
                .then_with(|| {
                    b_match
                        .shared_rel_file_count
                        .cmp(&a_match.shared_rel_file_count)
                })
                .then_with(|| {
                    b_state
                        .shared_file_name_weight
                        .partial_cmp(&a_state.shared_file_name_weight)
                        .unwrap_or(Ordering::Equal)
                })
                .then_with(|| {
                    b_state
                        .shared_file_name_count
                        .cmp(&a_state.shared_file_name_count)
                })
                .then_with(|| {
                    b_state
                        .shared_hash_weight
                        .partial_cmp(&a_state.shared_hash_weight)
                        .unwrap_or(Ordering::Equal)
                })
                .then_with(|| b_state.shared_hash_count.cmp(&a_state.shared_hash_count))
                .then_with(|| {
                    b_state
                        .shared_archive_signature_weight
                        .partial_cmp(&a_state.shared_archive_signature_weight)
                        .unwrap_or(Ordering::Equal)
                })
                .then_with(|| {
                    b_state
                        .shared_archive_signature_count
                        .cmp(&a_state.shared_archive_signature_count)
                })
                .then_with(|| {
                    b_state
                        .shared_normalized_file_name_weight
                        .partial_cmp(&a_state.shared_normalized_file_name_weight)
                        .unwrap_or(Ordering::Equal)
                })
                .then_with(|| {
                    b_state
                        .shared_normalized_file_name_count
                        .cmp(&a_state.shared_normalized_file_name_count)
                })
                .then_with(|| {
                    b_state
                        .shared_binaryity_weight
                        .partial_cmp(&a_state.shared_binaryity_weight)
                        .unwrap_or(Ordering::Equal)
                })
                .then_with(|| {
                    b_state
                        .shared_binaryity_count
                        .cmp(&a_state.shared_binaryity_count)
                })
                .then_with(|| {
                    b_state
                        .shared_archive_family_weight
                        .partial_cmp(&a_state.shared_archive_family_weight)
                        .unwrap_or(Ordering::Equal)
                })
                .then_with(|| {
                    b_state
                        .shared_archive_family_count
                        .cmp(&a_state.shared_archive_family_count)
                })
                .then_with(|| {
                    b_state
                        .shared_language_weight
                        .partial_cmp(&a_state.shared_language_weight)
                        .unwrap_or(Ordering::Equal)
                })
                .then_with(|| {
                    b_state
                        .shared_language_count
                        .cmp(&a_state.shared_language_count)
                })
                .then_with(|| {
                    b_state
                        .shared_size_class_weight
                        .partial_cmp(&a_state.shared_size_class_weight)
                        .unwrap_or(Ordering::Equal)
                })
                .then_with(|| {
                    b_state
                        .shared_size_class_count
                        .cmp(&a_state.shared_size_class_count)
                })
                .then_with(|| {
                    b_state
                        .shared_normalized_parent_folder_weight
                        .partial_cmp(&a_state.shared_normalized_parent_folder_weight)
                        .unwrap_or(Ordering::Equal)
                })
                .then_with(|| {
                    b_state
                        .shared_normalized_parent_folder_count
                        .cmp(&a_state.shared_normalized_parent_folder_count)
                })
                .then_with(|| {
                    b_state
                        .shared_file_ext_weight
                        .partial_cmp(&a_state.shared_file_ext_weight)
                        .unwrap_or(Ordering::Equal)
                })
                .then_with(|| {
                    b_state
                        .shared_ext_stem_weight
                        .partial_cmp(&a_state.shared_ext_stem_weight)
                        .unwrap_or(Ordering::Equal)
                })
                .then_with(|| {
                    b_state
                        .shared_file_stem_weight
                        .partial_cmp(&a_state.shared_file_stem_weight)
                        .unwrap_or(Ordering::Equal)
                })
                .then_with(|| {
                    b_state
                        .shared_folder_weight
                        .partial_cmp(&a_state.shared_folder_weight)
                        .unwrap_or(Ordering::Equal)
                })
                .then_with(|| b_match.right_folder.cmp(&a_match.right_folder))
        });
        ranked.truncate(top_k);
        output.extend(ranked.into_iter().map(|(item, _)| item));
    }

    output.sort_by(|a, b| {
        a.left_folder
            .cmp(&b.left_folder)
            .then_with(|| {
                b.overlap_ratio
                    .partial_cmp(&a.overlap_ratio)
                    .unwrap_or(Ordering::Equal)
            })
            .then_with(|| {
                b.overlap_weight
                    .partial_cmp(&a.overlap_weight)
                    .unwrap_or(Ordering::Equal)
            })
            .then_with(|| b.shared_rel_file_count.cmp(&a.shared_rel_file_count))
            .then_with(|| a.right_folder.cmp(&b.right_folder))
    });
    output
}

fn build_dossier_csv(matches: &[DossierMatch]) -> String {
    let mut csv = String::new();
    csv.push_str(
        "left_folder,right_folder,overlap_weight,left_weight,right_weight,overlap_ratio,shared_rel_file_count,shared_exact_file_name_count,shared_normalized_file_name_count,shared_file_stem_count,shared_file_ext_count,shared_ext_stem_count,shared_hash_count,shared_folder_token_count,shared_normalized_parent_folder_count,shared_binaryity_count,shared_archive_family_count,shared_language_count,shared_size_class_count,confidence_tier\n",
    );

    for item in matches {
        let _ = std::fmt::Write::write_fmt(
            &mut csv,
            format_args!(
                "{},{},{:.4},{:.4},{:.4},{:.6},{},{},{},{},{},{},{},{},{},{},{},{},{},{}\n",
                csv_escape(&item.left_folder),
                csv_escape(&item.right_folder),
                item.overlap_weight,
                item.left_weight,
                item.right_weight,
                item.overlap_ratio,
                item.shared_rel_file_count,
                item.shared_exact_file_name_count,
                item.shared_normalized_file_name_count,
                item.shared_file_stem_count,
                item.shared_file_ext_count,
                item.shared_ext_stem_count,
                item.shared_hash_count,
                item.shared_folder_token_count,
                item.shared_normalized_parent_folder_count,
                item.shared_binaryity_count,
                item.shared_archive_family_count,
                item.shared_language_count,
                item.shared_size_class_count,
                item.confidence_tier.as_str()
            ),
        );
    }
    csv
}

fn build_dossier_actions_csv(matches: &[DossierMatch]) -> String {
    let mut csv = String::new();
    csv.push_str(
        "left_folder,rank,right_folder,confidence_tier,next_action,overlap_ratio,shared_hash_count,shared_normalized_file_name_count,shared_rel_file_count\n",
    );

    let mut last_left = "";
    let mut rank_for_left = 0usize;
    for item in matches {
        if item.left_folder != last_left {
            last_left = &item.left_folder;
            rank_for_left = 1;
        } else {
            rank_for_left += 1;
        }
        let _ = std::fmt::Write::write_fmt(
            &mut csv,
            format_args!(
                "{},{},{},{},{},{:.6},{},{},{}\n",
                csv_escape(&item.left_folder),
                rank_for_left,
                csv_escape(&item.right_folder),
                item.confidence_tier.as_str(),
                item.confidence_tier.next_action(),
                item.overlap_ratio,
                item.shared_hash_count,
                item.shared_normalized_file_name_count,
                item.shared_rel_file_count
            ),
        );
    }
    csv
}

fn keep_top_candidate_per_left(matches: &[DossierMatch]) -> Vec<DossierMatch> {
    let mut seen_left = HashSet::new();
    let mut out = Vec::new();
    for item in matches {
        if seen_left.insert(item.left_folder.clone()) {
            out.push(item.clone());
        }
    }
    out
}

fn build_file_fingerprint_profile(
    rel_path: &str,
    file_type: &str,
    size: u64,
    fast_hash: Option<&str>,
) -> FileFingerprintProfile {
    let is_symlink = file_type == "symlink";
    let path = Path::new(rel_path);
    let file_name = path
        .file_name()
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_default()
        .to_lowercase();
    let folder = normalize_fingerprint_folder(rel_path);
    if is_symlink {
        return FileFingerprintProfile {
            normalized_name: "symlink".to_string(),
            normalized_folder: folder,
            ext: String::new(),
            is_binary: false,
            is_archive: false,
            archive_family: None,
            language: "symlink".to_string(),
            size_class: infer_size_class(0),
            binary_signature: None,
            binary_descriptor: None,
            text_signature: None,
            archive_signature: None,
        };
    }

    let normalized_name = normalize_fingerprint_name(&file_name);
    let ext = path
        .extension()
        .map(|value| value.to_string_lossy().to_ascii_lowercase());
    let archive_family = infer_archive_family(&file_name);
    let is_archive = archive_family.is_some();
    let is_binary = is_binary_path(&file_type, &file_name, ext.as_deref(), is_archive);
    let language = infer_source_language(rel_path);
    let size_class = infer_size_class(size);
    let archive_signature = archive_family
        .as_deref()
        .and_then(|family| infer_archive_payload_signature(rel_path, family));
    let binary_signature = if is_binary {
        Some(infer_binary_signature(
            rel_path,
            ext.as_deref(),
            archive_family.as_deref(),
            &size_class,
        ))
    } else {
        None
    };
    let binary_descriptor = if is_binary {
        Some(infer_binary_descriptor(
            rel_path,
            ext.as_deref(),
            archive_family.as_deref(),
            &size_class,
            fast_hash,
            None,
        ))
    } else {
        None
    };
    let text_signature = if !is_binary && language != "unknown" {
        Some(infer_text_signature(rel_path, &language, &normalized_name))
    } else {
        None
    };

    FileFingerprintProfile {
        normalized_name,
        normalized_folder: folder,
        ext: ext.unwrap_or_default(),
        is_binary,
        is_archive,
        archive_family,
        language,
        size_class,
        binary_signature,
        binary_descriptor,
        text_signature,
        archive_signature,
    }
}

fn normalize_fingerprint_name(file_name: &str) -> String {
    let path = Path::new(file_name);
    let stem = path
        .file_stem()
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_else(|| file_name.to_string());
    let normalized = normalize_fingerprint_token_text(&stem);
    if normalized.is_empty() {
        return "unnamed".to_string();
    }
    let mut tokens = normalized
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(normalize_fingerprint_token_text)
        .collect::<Vec<_>>();
    while let Some(token) = tokens.last() {
        if is_noise_fingerprint_token(token) {
            tokens.pop();
            continue;
        }
        break;
    }
    if tokens.is_empty() {
        "unnamed".to_string()
    } else {
        tokens.join("_")
    }
}

fn normalize_fingerprint_folder(rel_path: &str) -> String {
    let parent = Path::new(rel_path).parent();
    let mut segments = Vec::new();
    if let Some(parent) = parent {
        for segment in parent.iter() {
            let segment = segment.to_string_lossy();
            let normalized = normalize_fingerprint_name(&segment);
            if !normalized.is_empty() {
                segments.push(normalized);
            }
        }
    }
    if segments.is_empty() {
        ".".to_string()
    } else {
        segments.join("/")
    }
}

fn normalize_fingerprint_token_text(value: &str) -> String {
    let lowered = value.to_ascii_lowercase();
    let normalized = lowered
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { ' ' })
        .collect::<String>();
    let mut parts: Vec<&str> = normalized.split_whitespace().collect();
    while let Some(last) = parts.last() {
        if is_noise_fingerprint_token(last) {
            parts.pop();
            continue;
        }
        break;
    }
    parts.join(" ")
}

fn sanitize_descriptor_component(value: &str, max_len: usize) -> String {
    let normalized = normalize_fingerprint_name(value);
    if normalized.is_empty() || normalized == "unnamed" {
        return String::new();
    }
    normalized.chars().take(max_len).collect()
}

fn clamp_descriptor(value: String, max_len: usize) -> String {
    value.chars().take(max_len).collect()
}

fn is_low_signal_semantic_token(token: &str) -> bool {
    let trimmed = token.trim();
    if trimmed.len() < DESCRIPTOR_MIN_TOKEN_LEN {
        return true;
    }
    if trimmed.chars().all(|ch| ch.is_ascii_digit()) {
        return true;
    }
    matches!(
        trimmed,
        "tmp" | "var" | "key" | "val" | "cfg" | "data" | "test" | "misc" | "none"
    )
}

fn is_noise_fingerprint_token(token: &str) -> bool {
    let value = token.trim().to_ascii_lowercase();
    if value.is_empty() {
        return true;
    }
    if value == "final"
        || value == "copy"
        || value == "old"
        || value == "clean"
        || value == "backup"
        || value == "version"
    {
        return true;
    }
    if value.starts_with("v") {
        let suffix = &value[1..];
        if !suffix.is_empty() && suffix.chars().all(|ch| ch.is_ascii_digit()) {
            return true;
        }
    }
    if value.starts_with("rev")
        && value.len() > 3
        && value[3..].chars().all(|ch| ch.is_ascii_digit())
    {
        return true;
    }
    if value.starts_with('r') && value.len() > 1 && value[1..].chars().all(|ch| ch.is_ascii_digit())
    {
        return true;
    }
    if value.len() > 3 && value.chars().all(|ch| ch.is_ascii_digit()) {
        return true;
    }
    if value.len() == 8 && value.chars().all(|ch| ch.is_ascii_digit()) {
        return true;
    }
    if value.len() == 10 {
        let bytes = value.as_bytes();
        if (bytes[4] == b'-' || bytes[4] == b'_' || bytes[4] == b'.')
            && (bytes[7] == b'-' || bytes[7] == b'_' || bytes[7] == b'.')
            && value[..4].chars().all(|ch| ch.is_ascii_digit())
            && value[5..7].chars().all(|ch| ch.is_ascii_digit())
            && value[8..].chars().all(|ch| ch.is_ascii_digit())
        {
            return true;
        }
    }
    false
}

fn is_binary_extension(ext: &str) -> bool {
    BINARY_EXTENSIONS.contains(&ext)
}

fn is_binary_path(file_type: &str, rel_path: &str, ext: Option<&str>, is_archive: bool) -> bool {
    if file_type == "symlink" {
        return false;
    }
    if is_archive {
        return true;
    }

    if let Some(ext) = ext {
        if is_binary_extension(ext) {
            return true;
        }
    }

    let lower_path = rel_path.to_ascii_lowercase();
    BINARY_FOLDER_HINTS
        .iter()
        .any(|hint| lower_path.contains(hint))
}

fn build_folder_signatures(rows: &[FileRecord]) -> HashMap<String, FolderSignature> {
    build_folder_signatures_with_profiles(rows, &HashMap::new())
}

fn build_folder_signatures_with_profiles(
    rows: &[FileRecord],
    profiles: &HashMap<String, FileFingerprintProfile>,
) -> HashMap<String, FolderSignature> {
    let mut folders: HashMap<String, FolderSignature> = HashMap::new();
    for row in rows {
        let folder = folder_path_from_row(&row.rel_path);
        let rel_path = Path::new(&row.rel_path);
        let file_name = rel_path
            .file_name()
            .map(|value| value.to_string_lossy().to_string())
            .unwrap_or_default()
            .to_lowercase();
        let file_stem = rel_path
            .file_stem()
            .map(|value| value.to_string_lossy().to_string().to_lowercase())
            .unwrap_or_else(|| file_name.clone());
        let extension = rel_path
            .extension()
            .map(|value| value.to_string_lossy().to_string().to_lowercase())
            .unwrap_or_default();
        let profile = profiles.get(&row.rel_path).cloned().unwrap_or_else(|| {
            build_file_fingerprint_profile(
                &row.rel_path,
                &row.file_type,
                row.size,
                row.fast_hash.as_deref(),
            )
        });

        let extension_signature = dossier_extension_signature(&file_name, &extension);

        let signature = folders
            .entry(folder.clone())
            .or_insert_with(|| FolderSignature {
                path: folder.clone(),
                files: 0,
                total_bytes: 0,
                total_weight: 0.0,
                tokens: HashMap::new(),
            });

        signature.files += 1;
        signature.total_bytes += row.size;
        add_token(
            signature,
            format!("N:{file_name}"),
            DOSSIER_NAME_TOKEN_WEIGHT,
        );
        add_token(
            signature,
            format!("NF:{}", profile.normalized_name),
            DOSSIER_NORMALIZED_NAME_TOKEN_WEIGHT,
        );
        if !profile.normalized_folder.is_empty() {
            add_token(
                signature,
                format!("NFP:{}", profile.normalized_folder),
                DOSSIER_NORMALIZED_FOLDER_TOKEN_WEIGHT,
            );
        }
        add_token(
            signature,
            format!("S:{file_stem}"),
            DOSSIER_STEM_TOKEN_WEIGHT,
        );
        if !extension.is_empty() {
            add_token(
                signature,
                format!("E:{extension}"),
                DOSSIER_EXTENSION_TOKEN_WEIGHT,
            );
            add_token(
                signature,
                format!("ES:{file_stem}:{extension_signature}"),
                DOSSIER_EXTENSION_STEM_TOKEN_WEIGHT,
            );
        }
        if let Some(archive_ext) = extension_signature.strip_prefix(".") {
            add_token(
                signature,
                format!("E:{archive_ext}"),
                DOSSIER_EXTENSION_TOKEN_WEIGHT,
            );
            add_token(
                signature,
                format!("ES:{file_stem}:{archive_ext}"),
                DOSSIER_EXTENSION_STEM_TOKEN_WEIGHT,
            );
        }
        if profile.is_binary {
            add_token(
                signature,
                "BIN:binary".to_string(),
                DOSSIER_BINARYITY_TOKEN_WEIGHT,
            );
            if let Some(binary_signature) = &profile.binary_signature {
                add_token(
                    signature,
                    format!("BINSIG:{binary_signature}"),
                    DOSSIER_BINARY_SIGNATURE_TOKEN_WEIGHT,
                );
            }
            if let Some(binary_descriptor) = &profile.binary_descriptor {
                add_token(
                    signature,
                    format!("BINDESC:{binary_descriptor}"),
                    DOSSIER_BINARY_DESCRIPTOR_TOKEN_WEIGHT,
                );
            }
        } else {
            add_token(
                signature,
                "TEXT:text".to_string(),
                DOSSIER_BINARYITY_TOKEN_WEIGHT,
            );
            if let Some(text_signature) = &profile.text_signature {
                add_token(
                    signature,
                    format!("TEXTSIG:{text_signature}"),
                    DOSSIER_TEXT_SIGNATURE_TOKEN_WEIGHT,
                );
            }
        }
        if profile.language != "unknown" && !profile.language.is_empty() {
            add_token(
                signature,
                format!("LANG:{}", profile.language),
                DOSSIER_LANGUAGE_TOKEN_WEIGHT,
            );
        }
        if !profile.size_class.is_empty() {
            add_token(
                signature,
                format!("SIZE:{}", profile.size_class),
                DOSSIER_SIZE_CLASS_TOKEN_WEIGHT,
            );
            add_token(
                signature,
                format!("SZB:{}", profile.size_class),
                DOSSIER_SIZE_CLASS_TOKEN_WEIGHT,
            );
        }
        if let Some(archive_family) = &profile.archive_family {
            let token = format!("ARCH:{archive_family}");
            add_token(signature, token, DOSSIER_ARCHIVE_TOKEN_WEIGHT);
            add_token(
                signature,
                format!("ARCHFAM:{archive_family}"),
                DOSSIER_ARCHIVE_TOKEN_WEIGHT,
            );
            if let Some(archive_signature) = dossier_archive_signature(archive_family) {
                add_token(
                    signature,
                    format!("ARCHSIG:{archive_signature}"),
                    DOSSIER_ARCHIVE_SIGNATURE_TOKEN_WEIGHT,
                );
            }
            if let Some(archive_signature) = &profile.archive_signature {
                add_token(
                    signature,
                    format!("ARCHPAY:{archive_signature}"),
                    DOSSIER_ARCHIVE_SIGNATURE_TOKEN_WEIGHT,
                );
            }
            let virtual_path = build_virtual_archive_path(&row.rel_path);
            add_token(
                signature,
                format!("ARCHVIRT:{virtual_path}"),
                DOSSIER_ARCHIVE_SIGNATURE_TOKEN_WEIGHT,
            );
            let virtual_member = build_virtual_archive_member_path(&row.rel_path);
            add_token(
                signature,
                format!("ARCHMEM:{virtual_member}"),
                DOSSIER_ARCHIVE_SIGNATURE_TOKEN_WEIGHT * 0.6,
            );
            add_token(
                signature,
                format!("ARCHDEPTH:{}", archive_family_depth(archive_family)),
                DOSSIER_ARCHIVE_SIGNATURE_TOKEN_WEIGHT * 0.5,
            );
        }
        if let Some(hash) = &row.fast_hash {
            add_token(signature, format!("H:{hash}"), DOSSIER_HASH_TOKEN_WEIGHT);
        }
    }

    for (folder, signature) in folders.iter_mut() {
        if folder == "." || folder.is_empty() {
            continue;
        }
        let tokens = dossier_folder_tokens(folder);
        let mut running = String::new();
        for (depth, token) in tokens.iter().enumerate() {
            add_token(signature, format!("F:{token}"), DOSSIER_FOLDER_TOKEN_WEIGHT);
            add_token(
                signature,
                format!("FD:{depth}:{token}"),
                DOSSIER_FOLDER_DEPTH_TOKEN_WEIGHT,
            );
            if running.is_empty() {
                running.push_str(token);
            } else {
                running.push('/');
                running.push_str(token);
            }
            add_token(
                signature,
                format!("FP:{running}"),
                DOSSIER_FOLDER_PREFIX_TOKEN_WEIGHT,
            );
        }
    }

    folders
}

fn dossier_extension_signature(file_name: &str, extension: &str) -> String {
    let lower_name = file_name.to_ascii_lowercase();
    let mut best_match: Option<&str> = None;

    for archive_ext in ARCHIVE_EXTENSIONS {
        let archive_lower = archive_ext.to_ascii_lowercase();
        if lower_name.ends_with(&archive_lower) {
            if let Some(best) = best_match {
                if archive_lower.len() <= best.len() {
                    continue;
                }
            }
            best_match = Some(archive_ext);
        }
    }

    best_match.unwrap_or(extension).to_string()
}

fn dossier_archive_signature(file_name: &str) -> Option<String> {
    let signature = infer_archive_family(file_name)?;
    let mut parts: Vec<&str> = signature
        .split(|ch| ch == '.' || ch == '+')
        .filter(|part| !part.is_empty())
        .collect();
    if parts.len() < 2 {
        return None;
    }
    parts.retain(|part| !part.is_empty());
    if parts.is_empty() {
        return None;
    }
    let mut root = normalize_archive_token(parts[0]);
    if root == "raw" {
        root = "img".to_string();
    }
    Some(root)
}

fn normalize_archive_token(token: &str) -> String {
    match token {
        "raw" => "img".to_string(),
        value => value.to_string(),
    }
}

fn infer_archive_family(file_name: &str) -> Option<String> {
    let lower_name = file_name.to_ascii_lowercase();
    let mut current_name = lower_name.as_str();
    let mut parts: Vec<String> = Vec::new();

    loop {
        let path = Path::new(current_name);
        let extension = path
            .extension()
            .map(|value| value.to_string_lossy().to_ascii_lowercase());

        let extension = match extension {
            Some(extension) => extension,
            None => break,
        };

        let signature = dossier_extension_signature(current_name, &extension);
        if signature == extension {
            break;
        }

        let signature_len = signature.len();
        if signature_len >= current_name.len() {
            break;
        }
        parts.push(signature.trim_start_matches('.').to_string());
        current_name = &current_name[..current_name.len() - signature_len];
    }

    if parts.is_empty() {
        None
    } else {
        parts.reverse();
        Some(parts.join("."))
    }
}

fn infer_size_class(size: u64) -> String {
    if size == 0 {
        "0b".to_string()
    } else if size <= 4 * 1024 {
        "small".to_string()
    } else if size <= 1024 * 1024 {
        "1m".to_string()
    } else if size <= 10 * 1024 * 1024 {
        "10m".to_string()
    } else if size <= 100 * 1024 * 1024 {
        "100m".to_string()
    } else if size <= 1024 * 1024 * 1024 {
        "1g".to_string()
    } else {
        "large".to_string()
    }
}

fn infer_binary_signature(
    rel_path: &str,
    ext: Option<&str>,
    archive_family: Option<&str>,
    size_class: &str,
) -> String {
    let family = archive_family
        .and_then(archive_payload_root_from_family)
        .or_else(|| ext.map(|value| value.trim_start_matches('.').to_string()))
        .map(|value| sanitize_descriptor_component(&value, DESCRIPTOR_MAX_COMPONENT_LEN))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "raw".to_string());
    let folder_hint = rel_path
        .replace('\\', "/")
        .split('/')
        .rev()
        .nth(1)
        .map(|value| sanitize_descriptor_component(&value, DESCRIPTOR_MAX_COMPONENT_LEN))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| ".".to_string());
    clamp_descriptor(
        format!("{family}:{size_class}:{folder_hint}"),
        DESCRIPTOR_MAX_COMPOSITE_LEN,
    )
}

fn infer_binary_descriptor(
    rel_path: &str,
    ext: Option<&str>,
    archive_family: Option<&str>,
    size_class: &str,
    fast_hash: Option<&str>,
    sampled_signature: Option<&str>,
) -> String {
    let family = archive_family
        .and_then(archive_payload_root_from_family)
        .or_else(|| ext.map(|value| value.trim_start_matches('.').to_string()))
        .map(|value| sanitize_descriptor_component(&value, DESCRIPTOR_MAX_COMPONENT_LEN))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "raw".to_string());
    let mut parent = rel_path.replace('\\', "/");
    if let Some((prefix, _)) = parent.rsplit_once('/') {
        parent = prefix.to_string();
    } else {
        parent.clear();
    }
    let parent_tail = parent
        .split('/')
        .filter(|segment| !segment.is_empty())
        .next_back()
        .map(|value| sanitize_descriptor_component(&value, DESCRIPTOR_MAX_COMPONENT_LEN))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| ".".to_string());
    let hash_hint = fast_hash
        .map(|value| value.chars().take(10).collect::<String>())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "nohash".to_string());
    if let Some(sampled_signature) = sampled_signature.filter(|value| !value.is_empty()) {
        return clamp_descriptor(
            format!("{family}:{size_class}:{parent_tail}:{hash_hint}:s{sampled_signature}"),
            DESCRIPTOR_MAX_COMPOSITE_LEN,
        );
    }
    clamp_descriptor(
        format!("{family}:{size_class}:{parent_tail}:{hash_hint}"),
        DESCRIPTOR_MAX_COMPOSITE_LEN,
    )
}

fn infer_binary_sample_signature_from_file(path: &Path, size: u64) -> Option<String> {
    if size == 0 || size > BINARY_DESCRIPTOR_MAX_SAMPLE_FILE_BYTES {
        return None;
    }
    let mut file = File::open(path).ok()?;
    let chunk_len = size.min(BINARY_DESCRIPTOR_SAMPLE_CHUNK_BYTES as u64) as usize;
    let mut offsets = vec![0u64];
    if size > 1 {
        offsets.push(size / 3);
        offsets.push((2 * size) / 3);
        offsets.push(size.saturating_sub(chunk_len as u64));
    }
    offsets.sort_unstable();
    offsets.dedup();
    if offsets.len() > BINARY_DESCRIPTOR_SAMPLE_POINTS {
        offsets.truncate(BINARY_DESCRIPTOR_SAMPLE_POINTS);
    }

    let mut state: u64 = 0xcbf29ce484222325;
    let mut total_read = 0usize;
    for offset in offsets {
        if file.seek(SeekFrom::Start(offset)).is_err() {
            return None;
        }
        let mut buf = vec![0u8; chunk_len];
        let read = file.read(&mut buf).ok()?;
        if read == 0 {
            continue;
        }
        total_read += read;
        for value in offset.to_le_bytes() {
            state ^= value as u64;
            state = state.wrapping_mul(0x100000001b3);
        }
        for value in &buf[..read] {
            state ^= *value as u64;
            state = state.wrapping_mul(0x100000001b3);
        }
    }
    if total_read == 0 {
        return None;
    }
    Some(format!("{state:016x}"))
}

fn infer_text_signature(rel_path: &str, language: &str, normalized_name: &str) -> String {
    let semantic_name = if normalized_name.is_empty() {
        normalize_fingerprint_name(rel_path)
    } else {
        normalized_name.to_string()
    };
    format!("{language}:{semantic_name}")
}

fn infer_semantic_text_signature_from_file(
    path: &Path,
    profile: &FileFingerprintProfile,
) -> Result<Option<String>> {
    if profile.is_binary || profile.language == "unknown" || profile.language == "symlink" {
        return Ok(None);
    }
    let metadata = match fs::metadata(path) {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    const MAX_READ_BYTES: u64 = 256 * 1024;
    if metadata.len() > MAX_READ_BYTES {
        return Ok(None);
    }

    let mut content = String::new();
    File::open(path)
        .with_context(|| format!("failed to open {}", path.display()))?
        .read_to_string(&mut content)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let semantic = infer_semantic_text_signature_from_content(
        &profile.language,
        &profile.normalized_name,
        &content,
    );
    Ok(Some(semantic))
}

fn infer_semantic_text_signature_from_content(
    language: &str,
    normalized_name: &str,
    content: &str,
) -> String {
    let mut import_tokens = HashSet::new();
    let mut call_tokens = HashSet::new();
    let mut key_tokens = HashSet::new();
    let mut section_tokens = HashSet::new();

    for line in content.lines().take(512) {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("//") {
            continue;
        }

        if let Some(value) = parse_import_like(trimmed, language) {
            if is_low_signal_semantic_token(&value) {
                continue;
            }
            import_tokens.insert(value);
        }
        if let Some(value) = parse_call_like(trimmed, language) {
            call_tokens.insert(value);
        }
        if let Some(value) = parse_key_like(trimmed) {
            key_tokens.insert(value);
        }
        if let Some(value) = parse_section_like(trimmed) {
            section_tokens.insert(value);
        }
    }

    let mut import_vec = import_tokens.into_iter().collect::<Vec<_>>();
    let mut call_vec = call_tokens.into_iter().collect::<Vec<_>>();
    let mut key_vec = key_tokens.into_iter().collect::<Vec<_>>();
    let mut section_vec = section_tokens.into_iter().collect::<Vec<_>>();
    import_vec.sort();
    call_vec.sort();
    key_vec.sort();
    section_vec.sort();
    import_vec.truncate(4);
    call_vec.truncate(4);
    key_vec.truncate(4);
    section_vec.truncate(3);

    let semantic_name = if normalized_name.is_empty() {
        "unnamed".to_string()
    } else {
        sanitize_descriptor_component(normalized_name, DESCRIPTOR_MAX_COMPONENT_LEN)
    };
    let semantic_name = if semantic_name.is_empty() {
        "unnamed".to_string()
    } else {
        semantic_name
    };
    let mut parts = vec![format!("{language}:{semantic_name}")];
    if !import_vec.is_empty() {
        parts.push(format!("i:{}", import_vec.join("+")));
    }
    if !call_vec.is_empty() {
        parts.push(format!("f:{}", call_vec.join("+")));
    }
    if !key_vec.is_empty() {
        parts.push(format!("k:{}", key_vec.join("+")));
    }
    if !section_vec.is_empty() {
        parts.push(format!("s:{}", section_vec.join("+")));
    }
    clamp_descriptor(parts.join("|"), DESCRIPTOR_MAX_COMPOSITE_LEN)
}

fn parse_import_like(line: &str, language: &str) -> Option<String> {
    let normalize = |value: &str| {
        sanitize_descriptor_component(value, DESCRIPTOR_MAX_COMPONENT_LEN)
            .split('_')
            .next()
            .map(str::to_string)
            .unwrap_or_else(|| "unknown".to_string())
    };
    if language == "python" {
        if let Some(rest) = line.strip_prefix("import ") {
            return Some(normalize(rest.split(',').next()?.trim()));
        }
        if let Some(rest) = line.strip_prefix("from ") {
            return Some(normalize(rest.split_whitespace().next()?));
        }
    }
    if language == "rust" {
        if let Some(rest) = line.strip_prefix("use ") {
            return Some(normalize(rest.split("::").next()?));
        }
    }
    if language == "go" {
        if line.starts_with("import ") {
            let cleaned = line
                .trim_start_matches("import")
                .trim()
                .trim_matches('"')
                .trim_matches('`');
            if !cleaned.is_empty() {
                return Some(normalize(cleaned.rsplit('/').next()?));
            }
        }
    }
    if language == "c_family" && line.starts_with("#include") {
        let cleaned = line
            .trim_start_matches("#include")
            .trim()
            .trim_matches('<')
            .trim_matches('>')
            .trim_matches('"');
        if !cleaned.is_empty() {
            return Some(normalize(cleaned.split('/').next_back()?));
        }
    }
    if language == "javascript" {
        if let Some(rest) = line.strip_prefix("import ") {
            if let Some(idx) = rest.find(" from ") {
                let module = rest[(idx + 6)..].trim().trim_matches(';').trim_matches('"');
                if !module.is_empty() {
                    return Some(normalize(module.rsplit('/').next()?));
                }
            }
        }
        if let Some(rest) = line.split("require(").nth(1) {
            let module = rest
                .split(')')
                .next()
                .unwrap_or_default()
                .trim_matches('"')
                .trim_matches('\'');
            if !module.is_empty() {
                return Some(normalize(module.rsplit('/').next()?));
            }
        }
    }
    None
}

fn parse_call_like(line: &str, language: &str) -> Option<String> {
    if language == "python" && line.starts_with("def ") {
        let name = line.trim_start_matches("def ").split('(').next()?.trim();
        if !name.is_empty() {
            let normalized = sanitize_descriptor_component(name, DESCRIPTOR_MAX_COMPONENT_LEN);
            if !is_low_signal_semantic_token(&normalized) {
                return Some(normalized);
            }
        }
    }
    if language == "rust" && line.starts_with("fn ") {
        let name = line.trim_start_matches("fn ").split('(').next()?.trim();
        if !name.is_empty() {
            let normalized = sanitize_descriptor_component(name, DESCRIPTOR_MAX_COMPONENT_LEN);
            if !is_low_signal_semantic_token(&normalized) {
                return Some(normalized);
            }
        }
    }
    if language == "go" && line.starts_with("func ") {
        let name = line.trim_start_matches("func ").split('(').next()?.trim();
        if !name.is_empty() {
            let normalized = sanitize_descriptor_component(name, DESCRIPTOR_MAX_COMPONENT_LEN);
            if !is_low_signal_semantic_token(&normalized) {
                return Some(normalized);
            }
        }
    }
    if language == "c_family" && line.contains('(') && line.contains(')') && line.ends_with('{') {
        let prefix = line.split('(').next()?.trim();
        let name = prefix.split_whitespace().next_back()?;
        if !name.is_empty() {
            let normalized = sanitize_descriptor_component(name, DESCRIPTOR_MAX_COMPONENT_LEN);
            if !is_low_signal_semantic_token(&normalized) {
                return Some(normalized);
            }
        }
    }
    None
}

fn parse_key_like(line: &str) -> Option<String> {
    let assign = if line.contains('=') {
        line.split('=').next()
    } else if line.contains(':') {
        line.split(':').next()
    } else {
        None
    }?;
    let key = assign.trim().trim_matches('"').trim_matches('\'');
    if key.is_empty() || key.len() > DESCRIPTOR_MAX_KEY_TOKEN_LEN {
        return None;
    }
    let normalized = sanitize_descriptor_component(key, DESCRIPTOR_MAX_COMPONENT_LEN);
    if normalized.is_empty() || normalized == "unnamed" || is_low_signal_semantic_token(&normalized)
    {
        None
    } else {
        Some(normalized)
    }
}

fn parse_section_like(line: &str) -> Option<String> {
    if !(line.starts_with('[') && line.ends_with(']')) {
        return None;
    }
    let section = line.trim_matches('[').trim_matches(']').trim();
    if section.is_empty() {
        return None;
    }
    let normalized = sanitize_descriptor_component(section, DESCRIPTOR_MAX_COMPONENT_LEN);
    if normalized.is_empty() || is_low_signal_semantic_token(&normalized) {
        return None;
    }
    Some(normalized)
}

fn infer_archive_payload_signature(rel_path: &str, archive_family: &str) -> Option<String> {
    let payload = sanitize_descriptor_component(
        &archive_payload_root_from_family(archive_family)?,
        DESCRIPTOR_MAX_COMPONENT_LEN,
    );
    let stem =
        sanitize_descriptor_component(&build_archive_stem(rel_path), DESCRIPTOR_MAX_COMPONENT_LEN);
    if stem.is_empty() {
        Some(clamp_descriptor(payload, DESCRIPTOR_MAX_COMPOSITE_LEN))
    } else {
        Some(clamp_descriptor(
            format!("{payload}:{stem}"),
            DESCRIPTOR_MAX_COMPOSITE_LEN,
        ))
    }
}

fn archive_payload_root_from_family(archive_family: &str) -> Option<String> {
    let mut parts: Vec<&str> = archive_family
        .split(|ch| ch == '.' || ch == '+')
        .filter(|part| !part.is_empty())
        .collect();
    if parts.is_empty() {
        return None;
    }
    let mut root = normalize_archive_token(parts.remove(0));
    if root.is_empty() {
        root = "archive".to_string();
    }
    Some(root)
}

fn infer_source_language(file_name: &str) -> String {
    let path = Path::new(file_name);
    let extension = path
        .extension()
        .map(|value| value.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let lower_name = file_name.to_ascii_lowercase();

    let language = if lower_name.ends_with(".md")
        || lower_name.ends_with(".markdown")
        || lower_name.contains("/readme")
    {
        "markdown"
    } else if lower_name.ends_with(".json") {
        "json"
    } else if lower_name.ends_with(".toml")
        || lower_name.ends_with(".yaml")
        || lower_name.ends_with(".yml")
    {
        "config"
    } else if lower_name.ends_with(".xml") || lower_name.ends_with(".html") {
        "markup"
    } else if lower_name.ends_with(".rs") {
        "rust"
    } else if lower_name.ends_with(".py") || lower_name.ends_with(".pyi") {
        "python"
    } else if matches!(
        extension.as_str(),
        "c" | "h" | "cc" | "cpp" | "cxx" | "m" | "mm"
    ) {
        "c_family"
    } else if extension == "go" {
        "go"
    } else if matches!(
        extension.as_str(),
        "java" | "kt" | "rb" | "php" | "lua" | "pl"
    ) {
        "script"
    } else if matches!(
        extension.as_str(),
        "js" | "ts" | "jsx" | "tsx" | "vue" | "svelte"
    ) {
        "javascript"
    } else if matches!(
        extension.as_str(),
        "sh" | "bash" | "zsh" | "cmd" | "ps1" | "bat"
    ) {
        "shell"
    } else {
        "unknown"
    };

    language.to_string()
}

fn dossier_folder_tokens(folder: &str) -> Vec<String> {
    folder
        .replace('\\', "/")
        .split('/')
        .filter_map(|token| {
            let trimmed = token.trim().to_ascii_lowercase();
            if trimmed.is_empty() || trimmed == "." {
                None
            } else {
                Some(trimmed)
            }
        })
        .collect()
}

fn dossier_token_family(token: &str) -> DossierTokenFamily {
    if token == "BIN:binary" || token == "TEXT:text" {
        return DossierTokenFamily::Binaryity;
    }

    let Some((prefix, _)) = token.split_once(':') else {
        return DossierTokenFamily::Other;
    };
    match prefix {
        "N" => DossierTokenFamily::ExactFileName,
        "NF" => DossierTokenFamily::NormalizedFileName,
        "S" => DossierTokenFamily::FileStem,
        "E" => DossierTokenFamily::FileExtension,
        "ES" => DossierTokenFamily::ExtensionStem,
        "H" => DossierTokenFamily::Hash,
        "ARCH" | "ARCHFAM" => DossierTokenFamily::ArchiveFamily,
        "ARCHSIG" | "ARCHPAY" | "ARCHVIRT" | "ARCHMEM" | "ARCHDEPTH" => {
            DossierTokenFamily::ArchiveSignature
        }
        "LANG" | "TEXTSIG" => DossierTokenFamily::Language,
        "SIZE" | "SZB" => DossierTokenFamily::SizeClass,
        "BINSIG" => DossierTokenFamily::Binaryity,
        "NFP" => DossierTokenFamily::NormalizedFolder,
        "F" | "FD" | "FP" => DossierTokenFamily::Folder,
        _ => DossierTokenFamily::Other,
    }
}

fn build_folder_token_index(
    signatures: &HashMap<String, FolderSignature>,
) -> HashMap<String, Vec<(String, f64)>> {
    let mut index = HashMap::new();
    for signature in signatures.values() {
        for (token, weight) in &signature.tokens {
            let buckets = index.entry(token.clone()).or_insert_with(Vec::new);
            buckets.push((signature.path.clone(), *weight));
        }
    }
    index
}

fn add_token(signature: &mut FolderSignature, token: String, weight: f64) {
    let entry = signature.tokens.entry(token).or_insert(0.0);
    *entry += weight;
    signature.total_weight += weight;
}

fn folder_path_from_row(rel_path: &str) -> String {
    Path::new(rel_path)
        .parent()
        .map(path_to_slash)
        .unwrap_or_else(|| ".".to_string())
}

fn csv_escape(value: &str) -> String {
    let needs_quote = value.contains(',') || value.contains('"') || value.contains('\n');
    let escaped = value.replace('"', "\"\"");
    if needs_quote {
        format!("\"{escaped}\"")
    } else {
        escaped
    }
}

fn build_copy_missing_plan(
    left_db: &Path,
    right_db: &Path,
    left: &str,
    right: &str,
    policy: Option<&ExcludePolicy>,
) -> Result<CopyPlan> {
    let left_conn = open_db(left_db)?;
    let right_conn = open_db(right_db)?;

    let left_rows = load_label(&left_conn, left)?;
    let right_rows = load_label(&right_conn, right)?;

    let mut left_map: HashMap<String, FileRecord> = HashMap::with_capacity(left_rows.len());
    let mut right_map: HashMap<String, FileRecord> = HashMap::with_capacity(right_rows.len());

    for row in left_rows {
        left_map.insert(row.rel_path.clone(), row);
    }
    for row in right_rows {
        right_map.insert(row.rel_path.clone(), row);
    }

    let mut items = Vec::new();
    let mut bytes_to_copy = 0u64;
    let mut rel_paths: Vec<&String> = left_map.keys().collect();
    rel_paths.sort();

    for rel_path in rel_paths {
        if let Some(policy) = policy {
            if should_exclude_path(rel_path, policy) {
                continue;
            }
        }
        if right_map.contains_key(rel_path) {
            continue;
        }
        let row = left_map
            .get(rel_path)
            .expect("left_map key list and map should stay aligned");
        bytes_to_copy += row.size;
        items.push(CopyPlanItem {
            rel_path: row.rel_path.clone(),
            file_type: row.file_type.clone(),
            size: row.size,
            mtime_ns: row.mtime_ns,
            fast_hash: row.fast_hash.clone(),
        });
    }

    Ok(CopyPlan {
        mode: "copy-missing".to_string(),
        left_label: left.to_string(),
        right_label: right.to_string(),
        left_db: Some(left_db.display().to_string()),
        right_db: Some(right_db.display().to_string()),
        generated_at_ns: now_ns()?,
        summary: CopyPlanSummary {
            files_to_copy: items.len(),
            bytes_to_copy,
            left_files: left_map.len(),
            right_files: right_map.len(),
        },
        items,
    })
}

fn filter_plan_by_patterns(
    plan: &CopyPlan,
    include_patterns: &[PatternSpec],
    filter_exclude_patterns: &[PatternSpec],
) -> CopyPlan {
    if include_patterns.is_empty() && filter_exclude_patterns.is_empty() {
        return plan.clone();
    }

    let items: Vec<CopyPlanItem> = plan
        .items
        .iter()
        .filter(|item| {
            let include_match = include_patterns.is_empty()
                || include_patterns
                    .iter()
                    .any(|pattern| path_pattern_matches_spec(pattern, &item.rel_path));
            let exclude_match = filter_exclude_patterns
                .iter()
                .any(|pattern| path_pattern_matches_spec(pattern, &item.rel_path));
            include_match && !exclude_match
        })
        .cloned()
        .collect();

    let bytes_to_copy = items.iter().map(|item| item.size).sum();
    let mut filtered = plan.clone();
    filtered.summary.files_to_copy = items.len();
    filtered.summary.bytes_to_copy = bytes_to_copy;
    filtered.items = items;
    filtered
}

fn filter_plan_by_max_age(plan: &CopyPlan, max_age_ns: i64, now_ns: i64) -> CopyPlan {
    if max_age_ns <= 0 {
        return CopyPlan {
            summary: CopyPlanSummary {
                files_to_copy: 0,
                bytes_to_copy: 0,
                ..plan.summary.clone()
            },
            items: Vec::new(),
            ..plan.clone()
        };
    }

    let items: Vec<CopyPlanItem> = plan
        .items
        .iter()
        .filter(|item| now_ns.saturating_sub(item.mtime_ns).max(0) <= max_age_ns)
        .cloned()
        .collect();
    let bytes_to_copy = items.iter().map(|item| item.size).sum();
    let mut filtered = plan.clone();
    filtered.summary.files_to_copy = items.len();
    filtered.summary.bytes_to_copy = bytes_to_copy;
    filtered.items = items;
    filtered
}

fn filter_plan_by_files_from(plan: &CopyPlan, files_from_patterns: &[PatternSpec]) -> CopyPlan {
    if files_from_patterns.is_empty() {
        return plan.clone();
    }

    let items: Vec<CopyPlanItem> = plan
        .items
        .iter()
        .filter(|item| {
            files_from_patterns
                .iter()
                .any(|pattern| path_pattern_matches_spec(pattern, &item.rel_path))
        })
        .cloned()
        .collect();

    let bytes_to_copy = items.iter().map(|item| item.size).sum();
    let mut filtered = plan.clone();
    filtered.summary.files_to_copy = items.len();
    filtered.summary.bytes_to_copy = bytes_to_copy;
    filtered.items = items;
    filtered
}

fn format_bytes_human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn backup_existing_entry(existing_path: &Path, backup_dir: &Path, rel_path: &str) -> Result<()> {
    let backup_path = backup_dir.join(rel_path);
    if let Some(parent) = backup_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    if backup_path.exists() {
        let backup_is_symlink = std::fs::symlink_metadata(&backup_path)
            .map(|metadata| metadata.file_type().is_symlink())
            .unwrap_or(false);
        if backup_path.is_dir() && !backup_is_symlink {
            std::fs::remove_dir_all(&backup_path)
                .with_context(|| format!("failed to clear {}", backup_path.display()))?;
        } else {
            std::fs::remove_file(&backup_path)
                .with_context(|| format!("failed to clear {}", backup_path.display()))?;
        }
    }

    match std::fs::rename(existing_path, &backup_path) {
        Ok(()) => Ok(()),
        Err(err) if is_cross_device_link(&err) => {
            let metadata = std::fs::symlink_metadata(existing_path)
                .with_context(|| format!("failed to stat {}", existing_path.display()))?;
            if metadata.file_type().is_symlink() {
                let target = std::fs::read_link(existing_path)
                    .with_context(|| format!("failed to read {}", existing_path.display()))?;
                create_symlink(&target, &backup_path)?;
                std::fs::remove_file(existing_path)
                    .with_context(|| format!("failed to remove {}", existing_path.display()))?;
            } else {
                std::fs::copy(existing_path, &backup_path)
                    .with_context(|| format!("failed to back up {}", existing_path.display()))?;
                std::fs::remove_file(existing_path)
                    .with_context(|| format!("failed to remove {}", existing_path.display()))?;
            }
            Ok(())
        }
        Err(err) => Err(err).with_context(|| {
            format!(
                "failed to back up {} -> {}",
                existing_path.display(),
                backup_path.display()
            )
        }),
    }
}

fn is_cross_device_link(err: &std::io::Error) -> bool {
    err.raw_os_error() == Some(18)
}

fn path_pattern_matches_spec(pattern: &PatternSpec, rel_path: &str) -> bool {
    let dir_only = pattern.dir_only;
    let pattern = normalize_policy_path(&pattern.pattern);
    let rel_path = normalize_policy_path(rel_path);
    if pattern.is_empty() || rel_path.is_empty() {
        return false;
    }

    if !pattern.contains('*') && !pattern.contains('?') {
        return if dir_only {
            rel_path.starts_with(&(pattern + "/"))
        } else {
            rel_path == pattern || rel_path.starts_with(&(pattern + "/"))
        };
    }

    let pattern_parts: Vec<&str> = pattern.split('/').collect();
    let path_parts: Vec<&str> = rel_path.split('/').collect();
    if dir_only {
        if path_parts.len() < 2 {
            return false;
        }
        for index in 1..path_parts.len() {
            if match_path_components(&pattern_parts, &path_parts[..index]) {
                return true;
            }
        }
        false
    } else {
        match_path_components(&pattern_parts, &path_parts)
    }
}

fn match_path_components(pattern_parts: &[&str], path_parts: &[&str]) -> bool {
    if pattern_parts.is_empty() {
        return path_parts.is_empty();
    }

    if pattern_parts[0] == "**" {
        if pattern_parts.len() == 1 {
            return true;
        }
        for index in 0..=path_parts.len() {
            if match_path_components(&pattern_parts[1..], &path_parts[index..]) {
                return true;
            }
        }
        return false;
    }

    if path_parts.is_empty() {
        return false;
    }

    if !match_path_segment(pattern_parts[0], path_parts[0]) {
        return false;
    }

    match_path_components(&pattern_parts[1..], &path_parts[1..])
}

fn match_path_segment(pattern: &str, value: &str) -> bool {
    fn inner(pattern: &[u8], value: &[u8]) -> bool {
        match pattern.first() {
            None => value.is_empty(),
            Some(b'*') => {
                for index in 0..=value.len() {
                    if inner(&pattern[1..], &value[index..]) {
                        return true;
                    }
                }
                false
            }
            Some(b'?') => !value.is_empty() && inner(&pattern[1..], &value[1..]),
            Some(ch) => !value.is_empty() && *ch == value[0] && inner(&pattern[1..], &value[1..]),
        }
    }

    inner(pattern.as_bytes(), value.as_bytes())
}

fn plan_copy_missing_command(args: PlanCopyMissingArgs) -> Result<()> {
    let policy = load_exclude_policy(args.policy.as_deref())?;
    let plan = build_copy_missing_plan(
        &args.left_db,
        &args.right_db,
        &args.left,
        &args.right,
        Some(&policy),
    )?;

    if let Some(parent) = args.out_json.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(&plan)?;
    std::fs::write(&args.out_json, format!("{json}\n"))
        .with_context(|| format!("failed to write {}", args.out_json.display()))?;
    println!("{json}");
    Ok(())
}

fn resume_plan_command(args: ResumePlanArgs) -> Result<()> {
    let conn = open_db(&args.db)?;
    if let Some(path) = args.jsonl_out.as_ref() {
        let session = load_resume_session_meta(&conn, args.session_id.as_deref())?
            .ok_or_else(|| anyhow!("no resume session found"))?;
        let rows = load_resume_items_for_export(
            &conn,
            &session.session_id,
            args.only_failed,
            args.max_attempts,
        )?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)
            .with_context(|| format!("failed to open {}", path.display()))?;
        for row in rows {
            let payload = serde_json::to_vec(&row)?;
            file.write_all(&payload)?;
            file.write_all(b"\n")?;
        }
        eprintln!("[resume] wrote jsonl export: {}", path.display());
        return Ok(());
    }

    if args.prune_completed {
        let result =
            prune_resume_completed_rows(&conn, args.session_id.as_deref(), args.dry_run_prune)?;
        if args.vacuum && !args.dry_run_prune {
            conn.execute_batch("VACUUM")?;
        }
        let json = serde_json::to_string_pretty(&result)?;
        println!("{json}");
        return Ok(());
    }
    if args.list_sessions {
        let list = list_resume_sessions(&conn)?;
        let json = serde_json::to_string_pretty(&list)?;
        println!("{json}");
        return Ok(());
    }

    if args.stats {
        let session = load_resume_session_meta(&conn, args.session_id.as_deref())?
            .ok_or_else(|| anyhow!("no resume session found"))?;
        let stats = load_resume_session_stats(&conn, &session.session_id)?;
        let json = serde_json::to_string_pretty(&stats)?;
        println!("{json}");
        return Ok(());
    }

    let plan = build_resume_copy_plan(
        &args.db,
        args.session_id.as_deref(),
        args.only_failed,
        args.max_attempts,
    )?;
    if let Some(path) = args.out_json.as_ref() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let json = serde_json::to_string_pretty(&plan)?;
        std::fs::write(path, format!("{json}\n"))
            .with_context(|| format!("failed to write {}", path.display()))?;
        println!("{json}");
    } else if !args.execute {
        bail!("resume-plan requires --out-json unless --execute is set");
    }

    if args.execute {
        let filter_mode = if args.only_failed {
            "failed-only"
        } else {
            "pending+copying+failed"
        };
        let attempts_text = args
            .max_attempts
            .map(|value| value.to_string())
            .unwrap_or_else(|| "none".to_string());
        eprintln!(
            "[resume preflight] session={} files={} bytes={} filter={} max_attempts={} dry_run={} overwrite={}",
            args.session_id.as_deref().unwrap_or("latest"),
            plan.items.len(),
            plan.summary.bytes_to_copy,
            filter_mode,
            attempts_text,
            args.dry_run,
            args.overwrite
        );

        let source_root = resolve_copy_source_root(
            args.from
                .as_deref()
                .ok_or_else(|| anyhow!("--from is required with --execute"))?,
            "source",
        )?;
        let destination_root = resolve_copy_destination_root(
            args.to
                .as_deref()
                .ok_or_else(|| anyhow!("--to is required with --execute"))?,
        )?;
        let policy = load_exclude_policy(args.policy.as_deref())?;
        let started_at_ns = now_ns()?;
        let summary = execute_copy_missing_with_plan(
            &plan,
            CopyRunArgs {
                source_root,
                destination_root,
                backup_dir: None,
                overwrite: args.overwrite,
                dry_run: args.dry_run,
                stop_on_error: args.stop_on_error,
                log: args.log,
                progress_every: args.progress_every,
                size_only: false,
                hash: false,
                copy_links_as_files: false,
            },
            Some(&policy),
        )?;
        let elapsed_ns = now_ns()? - started_at_ns;
        if !args.dry_run {
            record_copy_run_stats(&plan, &summary, elapsed_ns);
        }
        let json = serde_json::to_string_pretty(&summary)?;
        println!("{json}");
    }

    Ok(())
}

fn execute_copy_missing_command(args: ExecuteCopyMissingArgs) -> Result<()> {
    let source_root = resolve_copy_source_root(&args.from, "source")?;
    let destination_root = resolve_copy_destination_root(&args.to)?;

    let plan_text = std::fs::read_to_string(&args.plan)
        .with_context(|| format!("failed to read {}", args.plan.display()))?;
    let plan: CopyPlan = serde_json::from_str(&plan_text)
        .with_context(|| format!("failed to parse plan {}", args.plan.display()))?;
    let policy = load_exclude_policy(args.policy.as_deref())?;
    let started_at_ns = now_ns()?;
    let summary = execute_copy_missing_with_plan(
        &plan,
        CopyRunArgs {
            source_root,
            destination_root,
            backup_dir: None,
            overwrite: args.overwrite,
            dry_run: args.dry_run,
            stop_on_error: args.stop_on_error,
            log: args.log,
            progress_every: args.progress_every,
            size_only: false,
            hash: false,
            copy_links_as_files: false,
        },
        Some(&policy),
    )?;
    let elapsed_ns = now_ns()? - started_at_ns;
    if !args.dry_run {
        record_copy_run_stats(&plan, &summary, elapsed_ns);
    }
    let json = serde_json::to_string_pretty(&summary)?;
    println!("{json}");
    Ok(())
}

fn execute_copy_missing_with_plan(
    plan: &CopyPlan,
    args: CopyRunArgs,
    policy: Option<&ExcludePolicy>,
) -> Result<CopyExecutionSummary> {
    if plan.mode != "copy-missing" {
        eprintln!(
            "[warn] executing plan with mode '{}' using copy-missing behavior",
            plan.mode
        );
    }

    run_copy_plan(plan, args, policy)
}

fn execute_plan_command(args: ExecutePlanArgs) -> Result<()> {
    let plan_text = std::fs::read_to_string(&args.plan)
        .with_context(|| format!("failed to read {}", args.plan.display()))?;
    let plan: CopyPlan = serde_json::from_str(&plan_text)
        .with_context(|| format!("failed to parse plan {}", args.plan.display()))?;
    let policy = load_exclude_policy(args.policy.as_deref())?;

    match plan.mode.as_str() {
        "copy-missing" => {
            let source_root = resolve_copy_source_root(&args.from, "source")?;
            let destination_root = resolve_copy_destination_root(&args.to)?;
            let started_at_ns = now_ns()?;
            let summary = execute_copy_missing_with_plan(
                &plan,
                CopyRunArgs {
                    source_root,
                    destination_root,
                    backup_dir: None,
                    overwrite: args.overwrite,
                    dry_run: args.dry_run,
                    stop_on_error: args.stop_on_error,
                    log: args.log,
                    progress_every: args.progress_every,
                    size_only: false,
                    hash: false,
                    copy_links_as_files: false,
                },
                Some(&policy),
            )?;
            let elapsed_ns = now_ns()? - started_at_ns;
            if !args.dry_run {
                record_copy_run_stats(&plan, &summary, elapsed_ns);
            }
            let json = serde_json::to_string_pretty(&summary)?;
            println!("{json}");
            Ok(())
        }
        other => bail!("unsupported plan mode: {}", other),
    }
}

fn sync_copy_missing_command(args: SyncCopyMissingArgs) -> Result<()> {
    let source_root = resolve_copy_source_root(&args.from, "source")?;
    let destination_root = resolve_copy_destination_root(&args.to)?;
    let policy = load_exclude_policy(args.policy.as_deref())?;
    let started_at_ns = now_ns()?;

    let plan = build_copy_missing_plan(
        &args.left_db,
        &args.right_db,
        &args.left,
        &args.right,
        Some(&policy),
    )?;
    if let Some(write_plan) = args.write_plan.as_ref() {
        write_copy_plan(write_plan, &plan)?;
    }

    let summary = execute_copy_missing_with_plan(
        &plan,
        CopyRunArgs {
            source_root,
            destination_root,
            backup_dir: None,
            overwrite: args.overwrite,
            dry_run: args.dry_run,
            stop_on_error: args.stop_on_error,
            log: args.log,
            progress_every: args.progress_every,
            size_only: false,
            hash: false,
            copy_links_as_files: false,
        },
        Some(&policy),
    )?;
    let elapsed_ns = now_ns()? - started_at_ns;
    if !args.dry_run {
        record_copy_run_stats(&plan, &summary, elapsed_ns);
    }
    let json = serde_json::to_string_pretty(&summary)?;
    println!("{json}");
    Ok(())
}

fn compat_copy_command(args: CompatCopyArgs, command: &str) -> Result<()> {
    let runtime = parse_compat_copy_flags(&args, command)?;
    if !runtime.accepted_link_flags.is_empty() {
        let dereference_links = runtime
            .accepted_link_flags
            .iter()
            .any(|flag| flag == "--copy-links" || flag == "--copy-unsafe-links");
        let link_mode = if dereference_links {
            "dereference symlinks into regular files"
        } else {
            "preserve symlinks"
        };
        eprintln!(
            "[nightindex {command}] compat symlink flags: {} ({link_mode})",
            runtime.accepted_link_flags.join(", ")
        );
    }
    if !runtime.unsupported_args.is_empty() {
        eprintln!(
            "[nightindex {command}] ignored/unsupported flags: {}",
            runtime.unsupported_args.join(", ")
        );
    }
    if runtime.inplace {
        eprintln!("[nightindex {command}] compat flag --inplace accepted");
    }
    if runtime.stats || runtime.human_readable || runtime.verbosity > 0 {
        let mut notes = Vec::<String>::new();
        if runtime.stats {
            notes.push("stats".to_string());
        }
        if runtime.human_readable {
            notes.push("human-readable".to_string());
        }
        if runtime.verbosity > 0 {
            notes.push(format!("verbose x{}", runtime.verbosity));
        }
        eprintln!(
            "[nightindex {command}] compat output flags: {}",
            notes.join(", ")
        );
    }
    if runtime.source_trailing_slash || runtime.destination_trailing_slash {
        eprintln!(
            "[nightindex {command}] positional roots are normalized; trailing slash is noted but does not change copy-root behavior"
        );
    }
    if !runtime.exclude_if_present.is_empty() {
        eprintln!(
            "[nightindex {command}] exclude-if-present markers: {}",
            runtime.exclude_if_present.join(", ")
        );
    }
    let mut policy = load_exclude_policy(runtime.policy.as_deref())?;

    if !runtime.exclude_prefixes.is_empty() {
        let excludes = normalize_excludes(&runtime.exclude_prefixes);
        for prefix in &excludes {
            if !policy
                .directory_prefixes
                .iter()
                .any(|existing| existing == prefix)
            {
                policy.directory_prefixes.push(prefix.clone());
            }
        }
        if !excludes.is_empty() {
            policy.enabled = true;
            eprintln!(
                "[nightindex {command}] active compatibility excludes: {}",
                excludes.join(", ")
            );
        }
    }
    if !runtime.include_patterns.is_empty() {
        eprintln!(
            "[nightindex {command}] include patterns: {}",
            runtime
                .include_patterns
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if !runtime.filter_exclude_patterns.is_empty() {
        eprintln!(
            "[nightindex {command}] filter excludes: {}",
            runtime
                .filter_exclude_patterns
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if !runtime.files_from_patterns.is_empty() {
        eprintln!(
            "[nightindex {command}] files-from allowlist: {}",
            runtime
                .files_from_patterns
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if let Some(max_age_ns) = runtime.max_age_ns {
        eprintln!("[nightindex {command}] max-age filter: {} ns", max_age_ns);
    }

    let work_root = {
        let root = std::env::temp_dir().join(format!(
            "nightindex-compat-{}-{}",
            std::process::id(),
            now_ns()?
        ));
        fs::create_dir_all(&root)
            .with_context(|| format!("failed to create {}", root.display()))?;
        root
    };
    let left_db = work_root.join("source.sqlite");
    let right_db = work_root.join("destination.sqlite");
    let left_label = "left_source".to_string();
    let right_label = "right_destination".to_string();
    let delete_mode = runtime.delete_mode;

    let source_root = resolve_copy_source_root(&runtime.source, "source")?;
    let destination_root = resolve_copy_destination_root(&runtime.destination)?;
    let backup_dir = if runtime.backup_requested {
        Some(
            runtime
                .backup_dir
                .clone()
                .unwrap_or_else(|| destination_root.join(".nightindex-backup")),
        )
    } else {
        None
    };

    scan_command(ScanArgs {
        db: left_db.clone(),
        label: left_label.clone(),
        root: source_root.clone(),
        exclude_prefixes: runtime.exclude_prefixes.clone(),
        exclude_if_present: runtime.exclude_if_present.clone(),
        policy: runtime.policy.clone(),
        hash: runtime.hash,
    })?;
    scan_command(ScanArgs {
        db: right_db.clone(),
        label: right_label.clone(),
        root: destination_root.clone(),
        exclude_prefixes: runtime.exclude_prefixes,
        exclude_if_present: runtime.exclude_if_present,
        policy: runtime.policy.clone(),
        hash: runtime.hash,
    })?;
    let mut delete_summary = DeleteExecutionSummary::default();
    if matches!(delete_mode, Some(DeleteMode::Before)) {
        let summary = run_delete_pass(
            &left_db,
            &right_db,
            &left_label,
            &right_label,
            DeleteRunArgs {
                destination_root: destination_root.clone(),
                backup_dir: backup_dir.clone(),
                dry_run: runtime.dry_run,
                stop_on_error: runtime.stop_on_error,
                log: runtime.log.clone(),
                progress_every: runtime.progress_every,
                delete_excluded: runtime.delete_excluded,
            },
            Some(&policy),
        )?;
        delete_summary.deleted_files += summary.deleted_files;
        delete_summary.deleted_bytes += summary.deleted_bytes;
        delete_summary.failed_files += summary.failed_files;
    }
    let plan = build_copy_missing_plan(
        &left_db,
        &right_db,
        &left_label,
        &right_label,
        Some(&policy),
    )?;
    let plan = filter_plan_by_patterns(
        &plan,
        &runtime.include_patterns,
        &runtime.filter_exclude_patterns,
    );
    let plan = filter_plan_by_files_from(&plan, &runtime.files_from_patterns);
    let plan = if let Some(max_age_ns) = runtime.max_age_ns {
        filter_plan_by_max_age(&plan, max_age_ns, now_ns()?)
    } else {
        plan
    };

    let mut summary = execute_copy_missing_with_plan(
        &plan,
        CopyRunArgs {
            source_root,
            destination_root: destination_root.clone(),
            backup_dir: backup_dir.clone(),
            overwrite: runtime.overwrite,
            dry_run: runtime.dry_run,
            stop_on_error: runtime.stop_on_error,
            log: runtime.log.clone(),
            progress_every: runtime.progress_every,
            size_only: runtime.size_only,
            hash: runtime.hash,
            copy_links_as_files: runtime
                .accepted_link_flags
                .iter()
                .any(|flag| flag == "--copy-links" || flag == "--copy-unsafe-links"),
        },
        Some(&policy),
    )?;
    if matches!(delete_mode, Some(DeleteMode::After)) {
        let summary = run_delete_pass(
            &left_db,
            &right_db,
            &left_label,
            &right_label,
            DeleteRunArgs {
                destination_root: destination_root.clone(),
                backup_dir: backup_dir.clone(),
                dry_run: runtime.dry_run,
                stop_on_error: runtime.stop_on_error,
                log: runtime.log.clone(),
                progress_every: runtime.progress_every,
                delete_excluded: runtime.delete_excluded,
            },
            Some(&policy),
        )?;
        delete_summary.deleted_files += summary.deleted_files;
        delete_summary.deleted_bytes += summary.deleted_bytes;
        delete_summary.failed_files += summary.failed_files;
    }
    summary.deleted_files += delete_summary.deleted_files;
    summary.deleted_bytes += delete_summary.deleted_bytes;
    summary.failed_files += delete_summary.failed_files;
    let cleanup = fs::remove_dir_all(&work_root);
    if let Err(err) = cleanup {
        eprintln!("[nightindex {command}] cleanup warning: {err}");
    }

    let json = serde_json::to_string_pretty(&summary)?;
    println!("{json}");
    if runtime.stats || runtime.human_readable || runtime.verbosity > 0 {
        let bytes = if runtime.human_readable {
            format_bytes_human(summary.copied_bytes)
        } else {
            summary.copied_bytes.to_string()
        };
        eprintln!(
            "[nightindex {command}] summary planned={} copied={} skipped_existing={} skipped_conflict={} overwritten={} missing_source={} failed={} copied_bytes={}",
            summary.planned_files,
            summary.copied_files,
            summary.skipped_existing,
            summary.skipped_conflict,
            summary.overwritten_files,
            summary.missing_source,
            summary.failed_files,
            bytes
        );
    }
    Ok(())
}

fn parse_compat_copy_flags(args: &CompatCopyArgs, command: &str) -> Result<CompatRuntime> {
    let mut parsed = CompatRuntime {
        source: PathBuf::new(),
        destination: PathBuf::new(),
        source_trailing_slash: false,
        destination_trailing_slash: false,
        overwrite: false,
        dry_run: false,
        stop_on_error: false,
        policy: None,
        hash: false,
        log: None,
        progress_every: 1000,
        size_only: false,
        delete_mode: None,
        delete_excluded: false,
        inplace: false,
        stats: false,
        human_readable: false,
        verbosity: 0,
        backup_requested: false,
        backup_dir: None,
        exclude_prefixes: Vec::new(),
        exclude_if_present: Vec::new(),
        include_patterns: Vec::new(),
        files_from_patterns: Vec::new(),
        filter_exclude_patterns: Vec::new(),
        max_age_ns: None,
        accepted_link_flags: Vec::new(),
        unsupported_args: Vec::new(),
    };
    let mut positionals: Vec<String> = Vec::new();

    let mut iter = args.compat_args.clone().into_iter();
    let mut unsupported_seen = HashSet::new();
    let next_value = |iter: &mut std::vec::IntoIter<String>, option: &str| -> Result<String> {
        iter.next().with_context(|| {
            format!("missing value for {option} in {command} compatibility parsing")
        })
    };

    while let Some(arg) = iter.next() {
        if arg == "--" {
            while let Some(value) = iter.next() {
                positionals.push(value);
            }
            break;
        }

        if let Some((key, value)) = arg.split_once('=') {
            if !key.starts_with("--") {
                positionals.push(arg);
                continue;
            }
            let key = key.replace('_', "-");
            let accepted_link_flag = matches!(
                key.as_str(),
                "--copy-links" | "--copy-unsafe-links" | "--links"
            );
            let inplace_flag = key == "--inplace";

            match key.as_str() {
                "--dry-run" => parsed.dry_run = true,
                "--ignore-existing" | "--update" => parsed.overwrite = false,
                "--checksum" | "--hash" => parsed.hash = true,
                "--stats" => parsed.stats = true,
                "--human-readable" => parsed.human_readable = true,
                "--backup" => parsed.backup_requested = true,
                "--copy-links"
                | "--copy-unsafe-links"
                | "--links"
                | "--perms"
                | "--times"
                | "--group"
                | "--owner"
                | "--chmod"
                | "--progress"
                | "--inplace" => {
                    if accepted_link_flag {
                        parsed.accepted_link_flags.push(key);
                    }
                    if inplace_flag {
                        parsed.inplace = true;
                    }
                }
                "--max-age" => {
                    parsed.max_age_ns = Some(parse_age_value(value).with_context(|| {
                        format!("invalid --max-age='{value}' in {command} compatibility parsing")
                    })?);
                }
                "--delete-excluded" => parsed.delete_excluded = true,
                "--log-file" | "--log" => parsed.log = Some(PathBuf::from(value)),
                "--policy" => parsed.policy = Some(PathBuf::from(value)),
                "--exclude" => parsed.exclude_prefixes.push(value.to_string()),
                "--exclude-from" => {
                    parse_exclude_file(value, &mut parsed.exclude_prefixes)
                        .with_context(|| format!("invalid --exclude-from value '{value}'"))?;
                }
                "--exclude-if-present" => {
                    parsed.exclude_if_present.push(normalize_policy_path(value));
                }
                "--progress-every" => {
                    parsed.progress_every = value
                        .parse::<usize>()
                        .map_err(|_| {
                            anyhow!(
                                "invalid --progress-every '{value}' in {command} compatibility parsing"
                            )
                        })?
                        .max(1);
                }
                "--size-only" | "--ignore-times" => {
                    parsed.size_only = true;
                }
                "--delete" => parsed.delete_mode = Some(DeleteMode::After),
                "--delete-before" | "--delete-during" => {
                    parsed.delete_mode = Some(DeleteMode::Before)
                }
                "--delete-after" => parsed.delete_mode = Some(DeleteMode::After),
                "--backup-dir" => parsed.backup_dir = Some(PathBuf::from(value)),
                "--files-from" => {
                    parse_include_file(value, &mut parsed.files_from_patterns)
                        .with_context(|| format!("invalid --files-from value '{value}'"))?;
                }
                "--filter" => parse_filter_rule(
                    value,
                    &mut parsed.include_patterns,
                    &mut parsed.filter_exclude_patterns,
                    &mut parsed.unsupported_args,
                    &mut unsupported_seen,
                ),
                "--include" => {
                    if let Some(pattern) = PatternSpec::parse(value) {
                        parsed.include_patterns.push(pattern);
                    }
                }
                "--include-from" => {
                    parse_include_file(value, &mut parsed.include_patterns)
                        .with_context(|| format!("invalid --include-from value '{value}'"))?;
                }
                "--filter-from" => {
                    parse_filter_file(
                        value,
                        &mut parsed.include_patterns,
                        &mut parsed.filter_exclude_patterns,
                        &mut parsed.unsupported_args,
                        &mut unsupported_seen,
                    )
                    .with_context(|| format!("invalid --filter-from value '{value}'"))?;
                }
                "--rsh" | "--ssh" | "--dry-run-mode" => {
                    parsed.unsupported_args.push(format!("{key}={value}"));
                }
                _ => parsed.unsupported_args.push(format!("{key}={value}")),
            }
            continue;
        }

        if let Some(raw_stripped) = arg.strip_prefix("--") {
            let stripped = raw_stripped.replace('_', "-");
            if stripped.is_empty() {
                positionals.push(arg);
                continue;
            }

            let option = format!("--{stripped}");

            match stripped.as_str() {
                "dry-run" => parsed.dry_run = true,
                "ignore-existing" | "update" => parsed.overwrite = false,
                "checksum" => parsed.hash = true,
                "stats" => parsed.stats = true,
                "human-readable" => parsed.human_readable = true,
                "backup" => parsed.backup_requested = true,
                "verbose" => parsed.verbosity = parsed.verbosity.saturating_add(1),
                "overwrite" => parsed.overwrite = true,
                "hash" => parsed.hash = true,
                "copy-links" | "copy-unsafe-links" | "links" | "perms" | "times" | "group"
                | "owner" | "progress" | "inplace" => {
                    if matches!(
                        stripped.as_str(),
                        "copy-links" | "copy-unsafe-links" | "links"
                    ) {
                        parsed.accepted_link_flags.push(option.clone());
                    }
                    if stripped == "inplace" {
                        parsed.inplace = true;
                    }
                }
                "stop-on-error" => parsed.stop_on_error = true,
                "size-only" | "ignore-times" => parsed.size_only = true,
                "log-file" | "log" => {
                    let value = next_value(&mut iter, &option)?;
                    parsed.log = Some(PathBuf::from(value));
                }
                "policy" => {
                    let value = next_value(&mut iter, &option)?;
                    parsed.policy = Some(PathBuf::from(value));
                }
                "exclude" => {
                    let value = next_value(&mut iter, &option)?;
                    parsed.exclude_prefixes.push(value);
                }
                "exclude-from" => {
                    let value = next_value(&mut iter, &option)?;
                    parse_exclude_file(&value, &mut parsed.exclude_prefixes)
                        .with_context(|| format!("invalid --exclude-from value '{value}'"))?;
                }
                "exclude-if-present" => {
                    let value = next_value(&mut iter, &option)?;
                    parsed
                        .exclude_if_present
                        .push(normalize_policy_path(&value));
                }
                "progress-every" => {
                    let value = next_value(&mut iter, &option)?;
                    parsed.progress_every = value
                        .parse::<usize>()
                        .with_context(|| format!("invalid --progress-every '{value}'"))?
                        .max(1);
                }
                "delete" => parsed.delete_mode = Some(DeleteMode::After),
                "delete-before" | "delete-during" => parsed.delete_mode = Some(DeleteMode::Before),
                "delete-after" => parsed.delete_mode = Some(DeleteMode::After),
                "delete-excluded" => parsed.delete_excluded = true,
                "backup-dir" => {
                    let value = next_value(&mut iter, &option)?;
                    parsed.backup_dir = Some(PathBuf::from(value));
                }
                "max-age" => {
                    let value = next_value(&mut iter, &option)?;
                    parsed.max_age_ns = Some(parse_age_value(&value).with_context(|| {
                        format!("invalid --max-age='{value}' in {command} compatibility parsing")
                    })?);
                }
                "files-from" => {
                    let value = next_value(&mut iter, &option)?;
                    parse_include_file(&value, &mut parsed.files_from_patterns)
                        .with_context(|| format!("invalid --files-from value '{value}'"))?;
                }
                "rsh" | "ssh" | "dry-run-mode" => {
                    let value = next_value(&mut iter, &option)?;
                    parsed
                        .unsupported_args
                        .push(format!("--{stripped}={value}"));
                }
                "filter" => {
                    let value = next_value(&mut iter, &option)?;
                    parse_filter_rule(
                        &value,
                        &mut parsed.include_patterns,
                        &mut parsed.filter_exclude_patterns,
                        &mut parsed.unsupported_args,
                        &mut unsupported_seen,
                    );
                }
                "include" => {
                    let value = next_value(&mut iter, &option)?;
                    if let Some(pattern) = PatternSpec::parse(&value) {
                        parsed.include_patterns.push(pattern);
                    }
                }
                "include-from" => {
                    let value = next_value(&mut iter, &option)?;
                    parse_include_file(&value, &mut parsed.include_patterns)
                        .with_context(|| format!("invalid --include-from value '{value}'"))?;
                }
                "filter-from" => {
                    let value = next_value(&mut iter, &option)?;
                    parse_filter_file(
                        &value,
                        &mut parsed.include_patterns,
                        &mut parsed.filter_exclude_patterns,
                        &mut parsed.unsupported_args,
                        &mut unsupported_seen,
                    )
                    .with_context(|| format!("invalid --filter-from value '{value}'"))?;
                }
                _ => parsed.unsupported_args.push(format!("--{stripped}")),
            }
            continue;
        }

        if arg.starts_with('-') {
            let short = &arg[1..];
            let mut index = 0usize;
            while index < short.len() {
                let flag = short.as_bytes()[index] as char;
                index += 1;

                let takes_value = matches!(flag, 'e' | 'f');
                if takes_value {
                    let value = if index < short.len() {
                        let value = short[index..].to_string();
                        index = short.len();
                        value
                    } else {
                        next_value(&mut iter, &format!("-{flag}"))?
                    };
                    match flag {
                        'e' => parsed.unsupported_args.push(format!("--rsh={value}")),
                        'f' => parse_filter_rule(
                            &value,
                            &mut parsed.include_patterns,
                            &mut parsed.filter_exclude_patterns,
                            &mut parsed.unsupported_args,
                            &mut unsupported_seen,
                        ),
                        _ => parsed.unsupported_args.push(format!("-{flag}={value}")),
                    }
                    continue;
                }

                match flag {
                    'n' => parsed.dry_run = true,
                    'u' => parsed.overwrite = false,
                    'c' => parsed.hash = true,
                    'v' => parsed.verbosity = parsed.verbosity.saturating_add(1),
                    'h' => parsed.human_readable = true,
                    'a' | 'r' | 'l' | 't' | 'p' | 'H' | 'L' | 'z' | 'R' | 'x' | 'q' | 'I' | 'S'
                    | 'k' | 'm' | 'D' | 'o' | 'g' | 'P' => {}
                    _ => parsed.unsupported_args.push(format!("-{flag}")),
                }
            }
            continue;
        }

        positionals.push(arg);
    }

    if positionals.len() < 2 {
        bail!("{command} requires <source> <destination>");
    }
    parsed.source = PathBuf::from(positionals[0].clone());
    parsed.destination = PathBuf::from(positionals[1].clone());
    parsed.source_trailing_slash = positionals[0].ends_with('/') || positionals[0].ends_with('\\');
    parsed.destination_trailing_slash =
        positionals[1].ends_with('/') || positionals[1].ends_with('\\');
    if positionals.len() > 2 {
        for extra in &positionals[2..] {
            parsed
                .unsupported_args
                .push(format!("extra positional: {extra}"));
        }
    }
    if parsed.backup_dir.is_some() {
        parsed.backup_requested = true;
    }
    parsed.progress_every = parsed.progress_every.max(1);
    Ok(parsed)
}

fn parse_exclude_file(path: &str, excludes: &mut Vec<String>) -> Result<()> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read exclude file {path}"))?;
    for line in text.lines() {
        let value = line.trim();
        if value.is_empty() || value.starts_with('#') {
            continue;
        }
        excludes.push(value.to_string());
    }
    Ok(())
}

fn push_unsupported_arg(output: &mut Vec<String>, seen: &mut HashSet<String>, value: String) {
    if seen.insert(value.clone()) {
        output.push(value);
    }
}

fn parse_include_file(path: &str, includes: &mut Vec<PatternSpec>) -> Result<()> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read include file {path}"))?;
    for line in text.lines() {
        let value = line.trim();
        if value.is_empty() || value.starts_with('#') {
            continue;
        }
        if let Some(pattern) = PatternSpec::parse(value) {
            includes.push(pattern);
        }
    }
    Ok(())
}

fn parse_filter_file(
    path: &str,
    filter_includes: &mut Vec<PatternSpec>,
    filter_excludes: &mut Vec<PatternSpec>,
    unsupported_args: &mut Vec<String>,
    unsupported_seen: &mut HashSet<String>,
) -> Result<()> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read filter file {path}"))?;
    for line in text.lines() {
        let rule = line.trim();
        if rule.is_empty() || rule.starts_with('#') {
            continue;
        }
        parse_filter_rule(
            rule,
            filter_includes,
            filter_excludes,
            unsupported_args,
            unsupported_seen,
        );
    }
    Ok(())
}

fn parse_filter_rule(
    rule: &str,
    filter_includes: &mut Vec<PatternSpec>,
    filter_excludes: &mut Vec<PatternSpec>,
    unsupported_args: &mut Vec<String>,
    unsupported_seen: &mut HashSet<String>,
) {
    let trimmed = rule.trim();
    if trimmed.is_empty() {
        return;
    }
    if let Some(rest) = trimmed.strip_prefix('+') {
        if let Some(pattern) = PatternSpec::parse(rest) {
            filter_includes.push(pattern);
            return;
        }
    } else if let Some(rest) = trimmed.strip_prefix('-') {
        if let Some(pattern) = PatternSpec::parse(rest) {
            filter_excludes.push(pattern);
            return;
        }
    }
    push_unsupported_arg(
        unsupported_args,
        unsupported_seen,
        format!("--filter={trimmed}"),
    );
}

fn resolve_copy_source_root(path: &Path, label: &str) -> Result<PathBuf> {
    let source_root = path
        .canonicalize()
        .with_context(|| format!("failed to resolve {label} root {}", path.display()))?;
    if !source_root.is_dir() {
        bail!("{label} root is not a directory: {}", source_root.display());
    }
    Ok(source_root)
}

fn resolve_copy_destination_root(path: &Path) -> Result<PathBuf> {
    let destination_root = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if !destination_root.exists() {
        fs::create_dir_all(&destination_root)
            .with_context(|| format!("failed to create {}", destination_root.display()))?;
    }
    Ok(destination_root)
}

fn write_copy_plan(path: &Path, plan: &CopyPlan) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let plan_json = serde_json::to_string_pretty(plan)?;
    std::fs::write(path, format!("{plan_json}\n"))
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

fn run_copy_plan(
    plan: &CopyPlan,
    args: CopyRunArgs,
    policy: Option<&ExcludePolicy>,
) -> Result<CopyExecutionSummary> {
    let progress_every = args.progress_every.max(1);
    let items_to_copy: Vec<&CopyPlanItem> = match policy {
        Some(policy) => plan
            .items
            .iter()
            .filter(|item| !should_exclude_path(&item.rel_path, policy))
            .collect(),
        None => plan.items.iter().collect(),
    };
    let mut log = match args.log.as_deref() {
        Some(path) => Some(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .with_context(|| format!("failed to open log {}", path.display()))?,
        ),
        None => None,
    };

    let mut copied = 0usize;
    let mut skipped_existing = 0usize;
    let mut skipped_conflict = 0usize;
    let mut overwritten_files = 0usize;
    let mut missing_source = 0usize;
    let mut failed = 0usize;
    let mut copied_bytes = 0u64;
    let started_at_ns = now_ns()?;
    let planned_total = items_to_copy.len();
    let planned_bytes = plan.summary.bytes_to_copy;
    let resume = ResumeRecorder::start(plan, &args, planned_total)?;
    eprintln!(
        "{}",
        format_start_line(
            &plan.mode,
            planned_total,
            planned_bytes,
            args.dry_run,
            args.overwrite
        )
    );
    let write_event = |log: &mut Option<std::fs::File>, event: &CopyEvent| -> Result<()> {
        if let Some(writer) = log {
            let payload = serde_json::to_vec(event).context("serialize copy event")?;
            writer.write_all(&payload)?;
            writer.write_all(b"\n")?;
        }
        Ok(())
    };
    for (index, item) in items_to_copy.iter().enumerate() {
        if let Some(recorder) = resume.as_ref() {
            recorder.mark_status(&item.rel_path, "copying", 0, None, true)?;
        }
        let source_path = args.source_root.join(&item.rel_path);
        let destination_path = args.destination_root.join(&item.rel_path);

        if !source_path.is_file() {
            missing_source += 1;
            failed += 1;
            write_event(
                &mut log,
                &CopyEvent {
                    schema_version: 2,
                    rel_path: item.rel_path.clone(),
                    action: CopyEventAction::SourceMissing,
                    existing_bytes: None,
                    bytes: 0,
                    dry_run: args.dry_run,
                    overwrite: args.overwrite,
                    reason: Some(format!("missing: {}", source_path.display())),
                },
            )?;
            if let Some(recorder) = resume.as_ref() {
                recorder.mark_status(&item.rel_path, "failed", 0, Some("missing source"), false)?;
            }
            if args.stop_on_error {
                bail!("missing source file: {}", source_path.display());
            } else {
                eprintln!("[err] missing source file: {}", source_path.display());
                continue;
            }
        }

        let mut destination_exists = false;
        let mut destination_metadata = None::<fs::Metadata>;
        let mut existing_bytes = None::<u64>;
        match fs::symlink_metadata(&destination_path) {
            Ok(metadata) => {
                destination_exists = true;
                existing_bytes = Some(metadata.len());
                destination_metadata = Some(metadata);
            }
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => {
                failed += 1;
                write_event(
                    &mut log,
                    &CopyEvent {
                        schema_version: 2,
                        rel_path: item.rel_path.clone(),
                        action: CopyEventAction::Fail,
                        existing_bytes: None,
                        bytes: 0,
                        dry_run: args.dry_run,
                        overwrite: args.overwrite,
                        reason: Some(format!("destination metadata unavailable: {}", err)),
                    },
                )?;
                if let Some(recorder) = resume.as_ref() {
                    recorder.mark_status(
                        &item.rel_path,
                        "failed",
                        0,
                        Some("destination metadata unavailable"),
                        false,
                    )?;
                }
                if args.stop_on_error {
                    bail!(
                        "failed reading destination metadata {}",
                        destination_path.display()
                    );
                } else {
                    eprintln!(
                        "[err] destination metadata unavailable: {}",
                        destination_path.display()
                    );
                    continue;
                }
            }
        }

        let is_symlink = item.file_type == "symlink";
        let source_metadata_override = if is_symlink && args.copy_links_as_files {
            Some(
                fs::metadata(&source_path)
                    .with_context(|| format!("failed to stat {}", source_path.display()))?,
            )
        } else {
            None
        };
        let source_size = source_metadata_override
            .as_ref()
            .map_or(item.size, |metadata| metadata.len());
        let source_mtime_ns = source_metadata_override
            .as_ref()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(system_time_to_ns)
            .unwrap_or(item.mtime_ns);
        let source_hash = if args.hash {
            if source_metadata_override.is_some() {
                Some(blake3_file(&source_path)?)
            } else {
                item.fast_hash.clone()
            }
        } else {
            item.fast_hash.clone()
        };
        let is_overwrite = args.overwrite;
        let mut action = CopyEventAction::Copy;

        if is_symlink && !args.copy_links_as_files {
            let target = match fs::read_link(&source_path) {
                Ok(target) => target,
                Err(err) => {
                    failed += 1;
                    write_event(
                        &mut log,
                        &CopyEvent {
                            schema_version: 2,
                            rel_path: item.rel_path.clone(),
                            action: CopyEventAction::Fail,
                            existing_bytes,
                            bytes: 0,
                            dry_run: args.dry_run,
                            overwrite: args.overwrite,
                            reason: Some(format!("failed reading symlink target: {}", err)),
                        },
                    )?;
                    if args.stop_on_error {
                        return Err(err)
                            .with_context(|| format!("failed reading {}", source_path.display()));
                    } else {
                        eprintln!(
                            "[err] failed reading symlink target: {}: {}",
                            source_path.display(),
                            err
                        );
                        continue;
                    }
                }
            };

            if destination_exists {
                if let Some(metadata) = destination_metadata.as_ref() {
                    if metadata.file_type().is_symlink() {
                        let destination_target = match fs::read_link(&destination_path) {
                            Ok(target) => target,
                            Err(err) => {
                                failed += 1;
                                write_event(
                                    &mut log,
                                    &CopyEvent {
                                        schema_version: 2,
                                        rel_path: item.rel_path.clone(),
                                        action: CopyEventAction::Fail,
                                        existing_bytes,
                                        bytes: 0,
                                        dry_run: args.dry_run,
                                        overwrite: args.overwrite,
                                        reason: Some(format!(
                                            "failed reading destination symlink target: {}",
                                            err
                                        )),
                                    },
                                )?;
                                if args.stop_on_error {
                                    return Err(err).with_context(|| {
                                        format!(
                                            "failed reading destination {}",
                                            destination_path.display()
                                        )
                                    });
                                } else {
                                    eprintln!(
                                        "[err] failed reading destination symlink target: {}: {}",
                                        destination_path.display(),
                                        err
                                    );
                                    continue;
                                }
                            }
                        };
                        if destination_target == target {
                            skipped_existing += 1;
                            write_event(
                                &mut log,
                                &CopyEvent {
                                    schema_version: 2,
                                    rel_path: item.rel_path.clone(),
                                    action: CopyEventAction::SkipExisting,
                                    existing_bytes,
                                    bytes: 0,
                                    dry_run: args.dry_run,
                                    overwrite: args.overwrite,
                                    reason: None,
                                },
                            )?;
                            if let Some(recorder) = resume.as_ref() {
                                recorder.mark_status(
                                    &item.rel_path,
                                    "skipped_existing",
                                    0,
                                    None,
                                    false,
                                )?;
                            }
                            continue;
                        }
                        if !is_overwrite {
                            skipped_conflict += 1;
                            write_event(
                                &mut log,
                                &CopyEvent {
                                    schema_version: 2,
                                    rel_path: item.rel_path.clone(),
                                    action: CopyEventAction::SkipConflict,
                                    existing_bytes,
                                    bytes: 0,
                                    dry_run: args.dry_run,
                                    overwrite: args.overwrite,
                                    reason: Some(format!(
                                        "destination conflict: existing symlink target {}",
                                        destination_target.display()
                                    )),
                                },
                            )?;
                            if let Some(recorder) = resume.as_ref() {
                                recorder.mark_status(
                                    &item.rel_path,
                                    "skipped_conflict",
                                    0,
                                    None,
                                    false,
                                )?;
                            }
                            continue;
                        }

                        overwritten_files += 1;
                        action = CopyEventAction::Overwrite;
                    } else {
                        skipped_conflict += 1;
                        write_event(
                            &mut log,
                            &CopyEvent {
                                schema_version: 2,
                                rel_path: item.rel_path.clone(),
                                action: CopyEventAction::SkipConflict,
                                existing_bytes,
                                bytes: 0,
                                dry_run: args.dry_run,
                                overwrite: args.overwrite,
                                reason: Some(
                                    "destination path exists and is not a symlink".to_string(),
                                ),
                            },
                        )?;
                        continue;
                    }
                }
            }

            if args.dry_run {
                copied += 1;

                if (index + 1) % progress_every == 0 {
                    println!("[dry-run] planned={} copied={}", index + 1, copied);
                }

                write_event(
                    &mut log,
                    &CopyEvent {
                        schema_version: 2,
                        rel_path: item.rel_path.clone(),
                        action,
                        existing_bytes,
                        bytes: 0,
                        dry_run: true,
                        overwrite: args.overwrite,
                        reason: None,
                    },
                )?;
                if let Some(recorder) = resume.as_ref() {
                    recorder.mark_status(
                        &item.rel_path,
                        if args.dry_run { "planned" } else { "done" },
                        0,
                        None,
                        false,
                    )?;
                }
                continue;
            }

            if let Some(parent) = destination_path.parent() {
                fs::create_dir_all(parent).with_context(|| {
                    format!(
                        "failed to create destination directory {}",
                        parent.display()
                    )
                })?;
            }
            if destination_exists {
                fs::remove_file(&destination_path)
                    .with_context(|| format!("failed to replace {}", destination_path.display()))?;
            }

            create_symlink(&target, &destination_path).with_context(|| {
                format!("failed to create symlink {}", destination_path.display())
            })?;
            copied += 1;
            copied_bytes += 0;
            if (index + 1) % progress_every == 0 {
                println!(
                    "[copy] {} / {} ({} bytes)",
                    index + 1,
                    items_to_copy.len(),
                    copied_bytes
                );
            }
            write_event(
                &mut log,
                &CopyEvent {
                    schema_version: 2,
                    rel_path: item.rel_path.clone(),
                    action,
                    existing_bytes,
                    bytes: 0,
                    dry_run: false,
                    overwrite: args.overwrite,
                    reason: None,
                },
            )?;
            continue;
        }

        if destination_exists {
            if let Some(metadata) = destination_metadata.as_ref() {
                if metadata.is_file() {
                    let mut same_file = false;
                    if metadata.len() == source_size {
                        let destination_mtime = metadata
                            .modified()
                            .ok()
                            .and_then(system_time_to_ns)
                            .filter(|mtime| *mtime == source_mtime_ns);

                        if destination_mtime.is_some() {
                            same_file = true;
                        } else if let Some(expected_hash) = source_hash.as_deref() {
                            same_file = blake3_file(&destination_path)? == expected_hash;
                        }
                        if args.size_only {
                            same_file = true;
                        }
                    }

                    if same_file {
                        skipped_existing += 1;
                        write_event(
                            &mut log,
                            &CopyEvent {
                                schema_version: 2,
                                rel_path: item.rel_path.clone(),
                                action: CopyEventAction::SkipExisting,
                                existing_bytes,
                                bytes: 0,
                                dry_run: args.dry_run,
                                overwrite: args.overwrite,
                                reason: None,
                            },
                        )?;
                        if let Some(recorder) = resume.as_ref() {
                            recorder.mark_status(
                                &item.rel_path,
                                "skipped_existing",
                                0,
                                None,
                                false,
                            )?;
                        }
                        continue;
                    }

                    if !is_overwrite {
                        skipped_conflict += 1;
                        write_event(
                            &mut log,
                            &CopyEvent {
                                schema_version: 2,
                                rel_path: item.rel_path.clone(),
                                action: CopyEventAction::SkipConflict,
                                existing_bytes,
                                bytes: source_size,
                                dry_run: args.dry_run,
                                overwrite: args.overwrite,
                                reason: Some(format!(
                                    "destination conflict: existing size {}",
                                    source_size
                                )),
                            },
                        )?;
                        if let Some(recorder) = resume.as_ref() {
                            recorder.mark_status(
                                &item.rel_path,
                                "skipped_conflict",
                                0,
                                None,
                                false,
                            )?;
                        }
                        continue;
                    }

                    overwritten_files += 1;
                    action = CopyEventAction::Overwrite;
                } else {
                    skipped_conflict += 1;
                    write_event(
                        &mut log,
                        &CopyEvent {
                            schema_version: 2,
                            rel_path: item.rel_path.clone(),
                            action: CopyEventAction::SkipConflict,
                            existing_bytes,
                            bytes: 0,
                            dry_run: args.dry_run,
                            overwrite: args.overwrite,
                            reason: Some(
                                "destination path exists and is not a regular file".to_string(),
                            ),
                        },
                    )?;
                    continue;
                }
            }
        }

        if args.dry_run {
            copied += 1;
            copied_bytes += source_size;

            if (index + 1) % progress_every == 0 {
                emit_copy_progress(
                    &mut log,
                    planned_total,
                    planned_bytes,
                    index + 1,
                    copied,
                    skipped_existing,
                    skipped_conflict,
                    overwritten_files,
                    missing_source,
                    failed,
                    copied_bytes,
                    started_at_ns,
                )?;
            }

            write_event(
                &mut log,
                &CopyEvent {
                    schema_version: 2,
                    rel_path: item.rel_path.clone(),
                    action,
                    existing_bytes,
                    bytes: source_size,
                    dry_run: true,
                    overwrite: args.overwrite,
                    reason: None,
                },
            )?;
            if let Some(recorder) = resume.as_ref() {
                recorder.mark_status(&item.rel_path, "planned", source_size, None, false)?;
            }
            continue;
        }

        if let Some(backup_dir) = args.backup_dir.as_ref() {
            backup_existing_entry(&destination_path, backup_dir, &item.rel_path)?;
        }

        if let Some(parent) = destination_path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "failed to create destination directory {}",
                    parent.display()
                )
            })?;
        }

        match copy_regular_file_via_temp(&source_path, &destination_path, args.overwrite) {
            Ok(RegularCopyOutcome::Copied(bytes_written)) => {
                copied += 1;
                copied_bytes += bytes_written;
                if (index + 1) % progress_every == 0 {
                    emit_copy_progress(
                        &mut log,
                        planned_total,
                        planned_bytes,
                        index + 1,
                        copied,
                        skipped_existing,
                        skipped_conflict,
                        overwritten_files,
                        missing_source,
                        failed,
                        copied_bytes,
                        started_at_ns,
                    )?;
                }
                write_event(
                    &mut log,
                    &CopyEvent {
                        schema_version: 2,
                        rel_path: item.rel_path.clone(),
                        action,
                        existing_bytes,
                        bytes: bytes_written,
                        dry_run: false,
                        overwrite: args.overwrite,
                        reason: None,
                    },
                )?;
                if let Some(recorder) = resume.as_ref() {
                    recorder.mark_status(&item.rel_path, "done", bytes_written, None, false)?;
                }
            }
            Err(err) => {
                failed += 1;
                if args.stop_on_error {
                    return Err(err)
                        .with_context(|| format!("failed copying {}", source_path.display()));
                }
                let err_text = err.to_string();
                write_event(
                    &mut log,
                    &CopyEvent {
                        schema_version: 2,
                        rel_path: item.rel_path.clone(),
                        action: CopyEventAction::Fail,
                        existing_bytes,
                        bytes: 0,
                        dry_run: false,
                        overwrite: args.overwrite,
                        reason: Some(err_text.clone()),
                    },
                )?;
                if let Some(recorder) = resume.as_ref() {
                    recorder.mark_status(&item.rel_path, "failed", 0, Some(&err_text), false)?;
                }
                eprintln!(
                    "[err] copy failed: {} -> {}: {}",
                    source_path.display(),
                    destination_path.display(),
                    err
                );
            }
            Ok(RegularCopyOutcome::LostRace) => {
                skipped_existing += 1;
                write_event(
                    &mut log,
                    &CopyEvent {
                        schema_version: 2,
                        rel_path: item.rel_path.clone(),
                        action: CopyEventAction::SkipExisting,
                        existing_bytes,
                        bytes: 0,
                        dry_run: false,
                        overwrite: args.overwrite,
                        reason: Some("destination appeared during copy".to_string()),
                    },
                )?;
                if let Some(recorder) = resume.as_ref() {
                    recorder.mark_status(
                        &item.rel_path,
                        "skipped_existing",
                        0,
                        Some("destination appeared during copy"),
                        false,
                    )?;
                }
            }
        }
    }

    let elapsed_ns = now_ns()? - started_at_ns;
    let summary = CopyExecutionSummary {
        mode: plan.mode.clone(),
        dry_run: args.dry_run,
        overwrite: args.overwrite,
        left_label: plan.left_label.clone(),
        right_label: plan.right_label.clone(),
        planned_files: items_to_copy.len(),
        copied_files: copied,
        skipped_existing,
        skipped_conflict,
        overwritten_files,
        missing_source,
        failed_files: failed,
        copied_bytes,
        deleted_files: 0,
        deleted_bytes: 0,
    };
    eprintln!("{}", format_summary_line(&summary, elapsed_ns));
    write_copy_summary_event(&mut log, &summary, elapsed_ns)?;
    if let Some(recorder) = resume.as_ref() {
        recorder.finish(copied, failed)?;
    }
    Ok(summary)
}

fn emit_copy_progress(
    log: &mut Option<std::fs::File>,
    planned_total: usize,
    planned_bytes: u64,
    completed_files: usize,
    copied_files: usize,
    skipped_existing: usize,
    skipped_conflict: usize,
    overwritten_files: usize,
    missing_source: usize,
    failed_files: usize,
    copied_bytes: u64,
    started_at_ns: i64,
) -> Result<()> {
    let elapsed_ns = now_ns()? - started_at_ns;
    let snapshot = CopyProgressSnapshot {
        planned_files: planned_total,
        planned_bytes,
        completed_files,
        copied_files,
        skipped_existing,
        skipped_conflict,
        overwritten_files,
        missing_source,
        failed_files,
        copied_bytes,
        elapsed_ns,
    };
    eprintln!("{}", format_progress_line(&snapshot));
    write_copy_progress_event(log, &snapshot)
}

fn copy_regular_file_via_temp(
    source_path: &Path,
    destination_path: &Path,
    overwrite: bool,
) -> Result<RegularCopyOutcome> {
    let mut stager = CopyStager::new();
    let stage = stager.stage(destination_path)?;
    let bytes_written = fs::copy(source_path, stage.temp_path()).with_context(|| {
        format!(
            "failed copying {} to temp {}",
            source_path.display(),
            stage.temp_path().display()
        )
    })?;

    match stage.finalize(destination_path, overwrite)? {
        CopyFinalizeOutcome::Committed => Ok(RegularCopyOutcome::Copied(bytes_written)),
        CopyFinalizeOutcome::SkippedConflict { .. } => Ok(RegularCopyOutcome::LostRace),
    }
}

#[cfg(unix)]
fn create_symlink(target: &Path, link_path: &Path) -> Result<()> {
    std::os::unix::fs::symlink(target, link_path)
        .with_context(|| format!("failed to create symlink {}", link_path.display()))
}

#[cfg(not(unix))]
fn create_symlink(_target: &Path, link_path: &Path) -> Result<()> {
    bail!(
        "symlink creation is not supported on this platform: {}",
        link_path.display()
    )
}

fn run_delete_pass(
    source_db: &Path,
    destination_db: &Path,
    source_label: &str,
    destination_label: &str,
    args: DeleteRunArgs,
    policy: Option<&ExcludePolicy>,
) -> Result<DeleteExecutionSummary> {
    let source_conn = open_readonly_db(source_db)?;
    let destination_conn = open_readonly_db(destination_db)?;
    let source_records = load_label(&source_conn, source_label)?;
    let destination_records = load_label(&destination_conn, destination_label)?;
    let source_paths: HashSet<String> =
        source_records.into_iter().map(|row| row.rel_path).collect();
    let delete_targets: Vec<FileRecord> = destination_records
        .into_iter()
        .filter(|row| {
            let excluded = policy
                .map(|policy| should_exclude_path(&row.rel_path, policy))
                .unwrap_or(false);
            (!source_paths.contains(&row.rel_path) && !excluded)
                || (args.delete_excluded && excluded)
        })
        .collect();

    let mut log = match args.log.as_deref() {
        Some(path) => Some(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .with_context(|| format!("failed to open log {}", path.display()))?,
        ),
        None => None,
    };

    let mut summary = DeleteExecutionSummary::default();
    for (index, item) in delete_targets.iter().enumerate() {
        let target_path = args.destination_root.join(&item.rel_path);
        if args.dry_run {
            summary.deleted_files += 1;
            summary.deleted_bytes += item.size;
            if (index + 1) % args.progress_every.max(1) == 0 {
                println!(
                    "[dry-run delete] planned={} deleted={}",
                    index + 1,
                    summary.deleted_files
                );
            }
            if let Some(writer) = log.as_mut() {
                let payload = serde_json::to_vec(&CopyEvent {
                    schema_version: 2,
                    rel_path: item.rel_path.clone(),
                    action: CopyEventAction::Delete,
                    existing_bytes: Some(item.size),
                    bytes: item.size,
                    dry_run: true,
                    overwrite: false,
                    reason: Some(
                        if policy
                            .map(|policy| should_exclude_path(&item.rel_path, policy))
                            .unwrap_or(false)
                        {
                            "excluded destination entry".to_string()
                        } else {
                            "destination-only entry".to_string()
                        },
                    ),
                })
                .context("serialize delete event")?;
                writer.write_all(&payload)?;
                writer.write_all(b"\n")?;
            }
            continue;
        }

        if let Some(backup_dir) = args.backup_dir.as_ref() {
            if let Err(err) = backup_existing_entry(&target_path, backup_dir, &item.rel_path) {
                summary.failed_files += 1;
                if args.stop_on_error {
                    return Err(err)
                        .with_context(|| format!("failed backing up {}", target_path.display()));
                }
                eprintln!("[err] backup failed: {}: {}", target_path.display(), err);
                continue;
            }
            summary.deleted_files += 1;
            summary.deleted_bytes += item.size;
            if (index + 1) % args.progress_every.max(1) == 0 {
                println!(
                    "[delete] {} / {} ({} bytes)",
                    index + 1,
                    delete_targets.len(),
                    summary.deleted_bytes
                );
            }
            if let Some(writer) = log.as_mut() {
                let payload = serde_json::to_vec(&CopyEvent {
                    schema_version: 2,
                    rel_path: item.rel_path.clone(),
                    action: CopyEventAction::Delete,
                    existing_bytes: Some(item.size),
                    bytes: item.size,
                    dry_run: false,
                    overwrite: false,
                    reason: Some(
                        if policy
                            .map(|policy| should_exclude_path(&item.rel_path, policy))
                            .unwrap_or(false)
                        {
                            "excluded destination entry".to_string()
                        } else {
                            "destination-only entry".to_string()
                        },
                    ),
                })
                .context("serialize delete event")?;
                writer.write_all(&payload)?;
                writer.write_all(b"\n")?;
            }
            continue;
        }

        match fs::remove_file(&target_path) {
            Ok(()) => {
                summary.deleted_files += 1;
                summary.deleted_bytes += item.size;
                if (index + 1) % args.progress_every.max(1) == 0 {
                    println!(
                        "[delete] {} / {} ({} bytes)",
                        index + 1,
                        delete_targets.len(),
                        summary.deleted_bytes
                    );
                }
                if let Some(writer) = log.as_mut() {
                    let payload = serde_json::to_vec(&CopyEvent {
                        schema_version: 2,
                        rel_path: item.rel_path.clone(),
                        action: CopyEventAction::Delete,
                        existing_bytes: Some(item.size),
                        bytes: item.size,
                        dry_run: false,
                        overwrite: false,
                        reason: Some(
                            if policy
                                .map(|policy| should_exclude_path(&item.rel_path, policy))
                                .unwrap_or(false)
                            {
                                "excluded destination entry".to_string()
                            } else {
                                "destination-only entry".to_string()
                            },
                        ),
                    })
                    .context("serialize delete event")?;
                    writer.write_all(&payload)?;
                    writer.write_all(b"\n")?;
                }
            }
            Err(err) if err.kind() == ErrorKind::NotFound => {
                summary.failed_files += 1;
                if args.stop_on_error {
                    bail!("missing delete target: {}", target_path.display());
                }
                eprintln!("[err] delete target missing: {}", target_path.display());
            }
            Err(err) => {
                summary.failed_files += 1;
                if args.stop_on_error {
                    return Err(err)
                        .with_context(|| format!("failed deleting {}", target_path.display()));
                }
                eprintln!("[err] delete failed: {}: {}", target_path.display(), err);
            }
        }
    }

    Ok(summary)
}

fn open_db(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let conn =
        Connection::open(path).with_context(|| format!("failed to open db {}", path.display()))?;
    conn.execute_batch(SCHEMA)?;
    ensure_file_fingerprint_columns(&conn)?;
    Ok(conn)
}

fn ensure_file_fingerprint_columns(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare("PRAGMA table_info(file_fingerprints)")?;
    let mut cols = HashSet::new();
    let mut rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    for col in rows.by_ref() {
        cols.insert(col?);
    }
    if !cols.contains("language") {
        conn.execute(
            "ALTER TABLE file_fingerprints ADD COLUMN language TEXT NOT NULL DEFAULT 'unknown'",
            (),
        )?;
    }
    if !cols.contains("size_class") {
        conn.execute(
            "ALTER TABLE file_fingerprints ADD COLUMN size_class TEXT NOT NULL DEFAULT 'large'",
            (),
        )?;
    }
    if !cols.contains("binary_signature") {
        conn.execute(
            "ALTER TABLE file_fingerprints ADD COLUMN binary_signature TEXT",
            (),
        )?;
    }
    if !cols.contains("binary_descriptor") {
        conn.execute(
            "ALTER TABLE file_fingerprints ADD COLUMN binary_descriptor TEXT",
            (),
        )?;
    }
    if !cols.contains("text_signature") {
        conn.execute(
            "ALTER TABLE file_fingerprints ADD COLUMN text_signature TEXT",
            (),
        )?;
    }
    if !cols.contains("archive_signature") {
        conn.execute(
            "ALTER TABLE file_fingerprints ADD COLUMN archive_signature TEXT",
            (),
        )?;
    }
    Ok(())
}

fn file_signature_cache_key(
    kind: &str,
    rel_path: &str,
    file_type: &str,
    size: u64,
    mtime_ns: i64,
    fast_hash: Option<&str>,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(kind.as_bytes());
    hasher.update(b"\0");
    hasher.update(rel_path.as_bytes());
    hasher.update(b"\0");
    hasher.update(file_type.as_bytes());
    hasher.update(b"\0");
    hasher.update(size.to_string().as_bytes());
    hasher.update(b"\0");
    hasher.update(mtime_ns.to_string().as_bytes());
    hasher.update(b"\0");
    if let Some(hash) = fast_hash {
        hasher.update(hash.as_bytes());
    }
    hasher.finalize().to_hex().to_string()
}

fn load_cached_file_fingerprint_profile(
    conn: &Connection,
    rel_path: &str,
    file_type: &str,
    size: u64,
    mtime_ns: i64,
    fast_hash: Option<&str>,
) -> Result<Option<FileFingerprintProfile>> {
    let cache_key = file_signature_cache_key(
        "file_fingerprint_profile",
        rel_path,
        file_type,
        size,
        mtime_ns,
        fast_hash,
    );
    let value_json: Option<String> = conn
        .query_row(
            "SELECT value_json FROM signature_cache WHERE cache_key = ?1",
            params![cache_key],
            |row| row.get(0),
        )
        .optional()?;
    value_json
        .map(|json| {
            serde_json::from_str(&json).context("failed to decode cached fingerprint profile")
        })
        .transpose()
}

fn store_cached_file_fingerprint_profile(
    conn: &Connection,
    rel_path: &str,
    file_type: &str,
    size: u64,
    mtime_ns: i64,
    fast_hash: Option<&str>,
    profile: &FileFingerprintProfile,
    computed_at: i64,
) -> Result<()> {
    let kind = "file_fingerprint_profile";
    let cache_key = file_signature_cache_key(kind, rel_path, file_type, size, mtime_ns, fast_hash);
    let value_json = serde_json::to_string(profile)?;
    conn.execute(
        r#"
        INSERT INTO signature_cache(cache_key, kind, rel_path, file_type, size, mtime_ns, fast_hash, value_json, computed_at)
        VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
        ON CONFLICT(cache_key) DO UPDATE SET
            value_json = excluded.value_json,
            computed_at = excluded.computed_at
        "#,
        params![
            cache_key,
            kind,
            rel_path,
            file_type,
            size,
            mtime_ns,
            fast_hash,
            value_json,
            computed_at
        ],
    )?;
    Ok(())
}

fn open_readonly_db(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("failed to open db {}", path.display()))?;
    Ok(conn)
}

fn load_label(conn: &Connection, label: &str) -> Result<Vec<FileRecord>> {
    let mut stmt = conn.prepare(
        "SELECT rel_path, file_type, size, mtime_ns, fast_hash FROM files WHERE label = ?1 ORDER BY rel_path",
    )?;
    let rows = stmt.query_map(params![label], |row| {
        Ok(FileRecord {
            rel_path: row.get(0)?,
            file_type: row.get(1)?,
            size: row.get(2)?,
            mtime_ns: row.get(3)?,
            fast_hash: row.get(4)?,
        })
    })?;

    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

fn load_file_fingerprint_profiles(
    conn: &Connection,
    label: &str,
) -> Result<HashMap<String, FileFingerprintProfile>> {
    let mut columns = HashSet::new();
    {
        let mut column_stmt = conn.prepare("PRAGMA table_info(file_fingerprints)")?;
        let mut col_rows = column_stmt.query_map([], |row| row.get::<_, String>(1))?;
        for col in col_rows.by_ref() {
            columns.insert(col?);
        }
    }

    let has_language = columns.contains("language");
    let has_size_class = columns.contains("size_class");
    let has_binary_descriptor = columns.contains("binary_descriptor");
    let has_signature_columns = columns.contains("binary_signature")
        && columns.contains("text_signature")
        && columns.contains("archive_signature");

    let mut check_stmt = conn
        .prepare("SELECT 1 FROM sqlite_master WHERE type='table' AND name='file_fingerprints'")?;
    let has_profiles = check_stmt.query_row([], |_| Ok(())).optional()?.is_some();
    if !has_profiles {
        return Ok(HashMap::new());
    }

    let query = if has_language && has_size_class && has_signature_columns && has_binary_descriptor
    {
        "SELECT rel_path, normalized_name, normalized_folder, ext, is_binary, is_archive, archive_family, language, size_class, binary_signature, binary_descriptor, text_signature, archive_signature FROM file_fingerprints WHERE label = ?1 ORDER BY rel_path"
    } else if has_language && has_size_class && has_signature_columns {
        "SELECT rel_path, normalized_name, normalized_folder, ext, is_binary, is_archive, archive_family, language, size_class, binary_signature, text_signature, archive_signature FROM file_fingerprints WHERE label = ?1 ORDER BY rel_path"
    } else if has_language && has_size_class {
        "SELECT rel_path, normalized_name, normalized_folder, ext, is_binary, is_archive, archive_family, language, size_class FROM file_fingerprints WHERE label = ?1 ORDER BY rel_path"
    } else {
        "SELECT rel_path, normalized_name, normalized_folder, ext, is_binary, is_archive, archive_family FROM file_fingerprints WHERE label = ?1 ORDER BY rel_path"
    };
    let mut stmt = conn.prepare(query)?;
    let rows = stmt.query_map(params![label], |row| {
        let is_binary: i64 = row.get(4)?;
        let is_archive: i64 = row.get(5)?;
        Ok(
            match (
                has_language,
                has_size_class,
                has_signature_columns,
                has_binary_descriptor,
            ) {
                (true, true, true, true) => (
                    row.get::<_, String>(0)?,
                    FileFingerprintProfile {
                        normalized_name: row.get(1)?,
                        normalized_folder: row.get(2)?,
                        ext: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                        is_binary: is_binary != 0,
                        is_archive: is_archive != 0,
                        archive_family: row.get(6)?,
                        language: row.get(7)?,
                        size_class: row.get(8)?,
                        binary_signature: row.get(9)?,
                        binary_descriptor: row.get(10)?,
                        text_signature: row.get(11)?,
                        archive_signature: row.get(12)?,
                    },
                ),
                (true, true, true, false) => (
                    row.get::<_, String>(0)?,
                    FileFingerprintProfile {
                        normalized_name: row.get(1)?,
                        normalized_folder: row.get(2)?,
                        ext: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                        is_binary: is_binary != 0,
                        is_archive: is_archive != 0,
                        archive_family: row.get(6)?,
                        language: row.get(7)?,
                        size_class: row.get(8)?,
                        binary_signature: row.get(9)?,
                        binary_descriptor: None,
                        text_signature: row.get(10)?,
                        archive_signature: row.get(11)?,
                    },
                ),
                (true, true, false, _) => (
                    row.get::<_, String>(0)?,
                    FileFingerprintProfile {
                        normalized_name: row.get(1)?,
                        normalized_folder: row.get(2)?,
                        ext: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                        is_binary: is_binary != 0,
                        is_archive: is_archive != 0,
                        archive_family: row.get(6)?,
                        language: row.get(7)?,
                        size_class: row.get(8)?,
                        binary_signature: None,
                        binary_descriptor: None,
                        text_signature: None,
                        archive_signature: None,
                    },
                ),
                _ => (
                    row.get::<_, String>(0)?,
                    FileFingerprintProfile {
                        normalized_name: row.get(1)?,
                        normalized_folder: row.get(2)?,
                        ext: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                        is_binary: is_binary != 0,
                        is_archive: is_archive != 0,
                        archive_family: row.get(6)?,
                        language: "unknown".to_string(),
                        size_class: "large".to_string(),
                        binary_signature: None,
                        binary_descriptor: None,
                        text_signature: None,
                        archive_signature: None,
                    },
                ),
            },
        )
    })?;

    let mut out = HashMap::with_capacity(rows.size_hint().0);
    for row in rows {
        let (rel_path, profile) = row?;
        out.insert(rel_path, profile);
    }
    Ok(out)
}

fn profile_cache_usage(
    rows: &[FileRecord],
    profiles: &HashMap<String, FileFingerprintProfile>,
) -> CacheUsageCounters {
    let mut hits = 0usize;
    let mut with_binary_descriptor = 0usize;
    let mut with_text_signature = 0usize;
    let mut with_archive_signature = 0usize;
    let mut with_any_descriptor = 0usize;

    for row in rows {
        if let Some(profile) = profiles.get(&row.rel_path) {
            hits += 1;
            let has_binary_descriptor = profile
                .binary_descriptor
                .as_deref()
                .is_some_and(|value| !value.is_empty());
            let has_text_signature = profile
                .text_signature
                .as_deref()
                .is_some_and(|value| !value.is_empty());
            let has_archive_signature = profile
                .archive_signature
                .as_deref()
                .is_some_and(|value| !value.is_empty());

            if has_binary_descriptor {
                with_binary_descriptor += 1;
            }
            if has_text_signature {
                with_text_signature += 1;
            }
            if has_archive_signature {
                with_archive_signature += 1;
            }
            if has_binary_descriptor || has_text_signature || has_archive_signature {
                with_any_descriptor += 1;
            }
        }
    }
    let total_rows = rows.len();
    let misses = total_rows.saturating_sub(hits);
    let coverage_ratio = if total_rows == 0 {
        0.0
    } else {
        (hits as f64) / (total_rows as f64)
    };
    let to_ratio = |count: usize| -> f64 {
        if hits == 0 {
            0.0
        } else {
            (count as f64) / (hits as f64)
        }
    };

    CacheUsageCounters {
        hits,
        misses,
        analytics: CacheUsageAnalytics {
            coverage: CacheCoverageMetrics {
                total_rows,
                profile_rows: hits,
                coverage_ratio,
            },
            descriptor_density: CacheDescriptorDensityMetrics {
                profiled_rows: hits,
                with_binary_descriptor,
                with_text_signature,
                with_archive_signature,
                with_any_descriptor,
                binary_descriptor_ratio: to_ratio(with_binary_descriptor),
                text_signature_ratio: to_ratio(with_text_signature),
                archive_signature_ratio: to_ratio(with_archive_signature),
                any_descriptor_ratio: to_ratio(with_any_descriptor),
            },
        },
    }
}

fn load_exclude_policy(path: Option<&Path>) -> Result<ExcludePolicy> {
    let mut policy = ExcludePolicy::empty();
    let Some(path) = path else {
        return Ok(policy);
    };

    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read policy file {}", path.display()))?;
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    let parsed: ExcludePolicy = match extension.to_ascii_lowercase().as_str() {
        "yml" | "yaml" => serde_yaml::from_str(&raw)
            .with_context(|| format!("invalid yaml policy {}", path.display()))?,
        "json5" => json5::from_str(&raw)
            .with_context(|| format!("invalid json5 policy {}", path.display()))?,
        "json" => serde_json::from_str(&raw).or_else(|json_err| {
            json5::from_str::<ExcludePolicy>(&raw).map_err(|json5_err| {
                anyhow::anyhow!(
                    "invalid json policy {}: json error: {}, json5 fallback error: {}",
                    path.display(),
                    json_err,
                    json5_err
                )
            })
        })?,
        _ => serde_yaml::from_str(&raw).or_else(|yaml_err| {
            serde_json::from_str(&raw).or_else(|json_err| {
                json5::from_str::<ExcludePolicy>(&raw).map_err(|json5_err| {
                    anyhow::anyhow!(
                        "failed to parse policy {} as yaml ({}) json ({}) or json5 ({})",
                        path.display(),
                        yaml_err,
                        json_err,
                        json5_err
                    )
                })
            })
        })?,
    };

    let mut prefixes = Vec::new();
    let mut seen_prefixes = HashSet::new();
    for prefix in parsed.directory_prefixes {
        let normalized = normalize_policy_path(&prefix);
        if !normalized.is_empty() && seen_prefixes.insert(normalized.clone()) {
            prefixes.push(normalized);
        }
    }

    let mut folder_name_additions = Vec::new();
    let mut seen_folder_name_additions = HashSet::new();
    for folder in parsed.folder_name_additions {
        let normalized = folder.trim().to_string();
        if !normalized.is_empty() && seen_folder_name_additions.insert(normalized.clone()) {
            folder_name_additions.push(normalized);
        }
    }

    let mut subtree_overrides = HashMap::new();
    for (prefix, folders) in parsed.subtree_overrides {
        let normalized_prefix = normalize_policy_path(&prefix);
        if normalized_prefix.is_empty() {
            continue;
        }
        let mut normalized_folders = Vec::new();
        let mut seen_folders = HashSet::new();
        for folder in folders {
            let normalized_folder = folder.trim().to_string();
            if !normalized_folder.is_empty() && seen_folders.insert(normalized_folder.clone()) {
                normalized_folders.push(normalized_folder);
            }
        }
        if !normalized_folders.is_empty() {
            subtree_overrides.insert(normalized_prefix, normalized_folders);
        }
    }

    policy.enabled = true;
    policy.directory_prefixes = prefixes;
    policy.folder_name_additions = folder_name_additions;
    policy.subtree_overrides = subtree_overrides;
    Ok(policy)
}

fn should_exclude_path(rel_path: &str, policy: &ExcludePolicy) -> bool {
    if !policy.enabled {
        return false;
    }

    let rel_path = normalize_policy_path(rel_path);
    if rel_path.is_empty() {
        return false;
    }

    if policy
        .directory_prefixes
        .iter()
        .any(|prefix| rel_path == *prefix || rel_path.starts_with(&(prefix.clone() + "/")))
    {
        return true;
    }

    let components: Vec<&str> = rel_path.split('/').collect();
    if components.is_empty() {
        return false;
    }

    let mut default_noise = DEFAULT_NOISE_DIRS
        .iter()
        .map(|name| name.to_string())
        .collect::<Vec<_>>();
    default_noise.extend(policy.folder_name_additions.iter().cloned());

    if contains_folder_noise(&components, None, &default_noise) {
        return true;
    }

    for (subtree_prefix, folders) in &policy.subtree_overrides {
        if rel_path == *subtree_prefix || rel_path.starts_with(&(subtree_prefix.clone() + "/")) {
            let mut subtree_noise = default_noise.clone();
            subtree_noise.extend(folders.iter().cloned());
            if contains_folder_noise(&components, Some(subtree_prefix), &subtree_noise) {
                return true;
            }
        }
    }

    false
}

fn normalize_policy_path(value: &str) -> String {
    value
        .trim()
        .trim_matches('/')
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("/")
}

fn contains_folder_noise(
    components: &[&str],
    subtree_prefix: Option<&str>,
    folders: &[String],
) -> bool {
    if components.is_empty() {
        return false;
    }
    let start_index = match subtree_prefix {
        Some(prefix) if !prefix.is_empty() => {
            let prefix_len = prefix.split('/').count();
            if components.len() < prefix_len {
                return false;
            }
            prefix_len
        }
        Some(_) | None => 0,
    };
    let folder_set: HashSet<&str> = folders.iter().map(String::as_str).collect();
    components[start_index..]
        .iter()
        .any(|component| folder_set.contains(component))
}

fn normalize_excludes(values: &[String]) -> Vec<String> {
    values
        .iter()
        .map(|value| normalize_policy_path(value))
        .filter(|value| !value.is_empty())
        .collect()
}

fn parse_age_value(value: &str) -> Result<i64> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        bail!("empty max-age value");
    }

    let (number_part, multiplier) = match trimmed.chars().last() {
        Some('s') | Some('S') => (&trimmed[..trimmed.len() - 1], 1u128),
        Some('m') | Some('M') => (&trimmed[..trimmed.len() - 1], 60u128),
        Some('h') | Some('H') => (&trimmed[..trimmed.len() - 1], 60u128 * 60),
        Some('d') | Some('D') => (&trimmed[..trimmed.len() - 1], 60u128 * 60 * 24),
        Some(ch) if ch.is_ascii_digit() => (trimmed, 1u128),
        _ => bail!("invalid max-age value '{value}'"),
    };

    let seconds = number_part
        .parse::<u128>()
        .with_context(|| format!("invalid max-age value '{value}'"))?;
    let nanos = seconds
        .checked_mul(multiplier)
        .and_then(|value| value.checked_mul(1_000_000_000))
        .context("max-age value is too large")?;
    if nanos > i64::MAX as u128 {
        bail!("max-age value is too large");
    }
    Ok(nanos as i64)
}

fn should_walk(
    path: &Path,
    root: &Path,
    policy: &ExcludePolicy,
    exclude_if_present: &[String],
) -> bool {
    if path == root {
        return !directory_has_marker(path, exclude_if_present);
    }
    let rel = match path.strip_prefix(root) {
        Ok(path) => path_to_slash(path),
        Err(_) => return true,
    };
    !should_exclude_path(&rel, policy) && !directory_has_marker(path, exclude_if_present)
}

fn directory_has_marker(path: &Path, markers: &[String]) -> bool {
    if markers.is_empty() || !path.is_dir() {
        return false;
    }
    markers.iter().any(|marker| path.join(marker).exists())
}

fn path_to_slash(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn now_ns() -> Result<i64> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("clock before unix epoch")?;
    Ok(duration.as_nanos() as i64)
}

fn system_time_to_ns(value: SystemTime) -> Option<i64> {
    value
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_nanos() as i64)
}

fn blake3_file(path: &Path) -> Result<String> {
    let mut file =
        File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = [0u8; 1024 * 1024];
    loop {
        let read = file
            .read(&mut buf)
            .with_context(|| format!("failed to read {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "nightindex-test-{}-{}-{}",
            std::process::id(),
            prefix,
            nanos
        ));
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    #[test]
    fn parse_compat_copy_flags_maps_supported_aliases() -> Result<()> {
        let root = temp_dir("compat");
        let exclude_file = root.join("excludes.txt");
        fs::write(&exclude_file, "cache\n#note\nnode_modules\n\n")?;

        let args = CompatCopyArgs {
            compat_args: vec![
                "--dry-run".into(),
                "-uc".into(),
                "--checksum".into(),
                "--progress-every".into(),
                "42".into(),
                "--log".into(),
                "/tmp/nightindex-compat.log".into(),
                "--exclude".into(),
                "tmp/cache".into(),
                "--exclude-from".into(),
                exclude_file.display().to_string(),
                "--ignore-times".into(),
                "source".into(),
                "dest".into(),
                "unused".into(),
            ],
        };

        let runtime = parse_compat_copy_flags(&args, "rsync")?;
        assert!(runtime.dry_run);
        assert!(runtime.hash);
        assert!(runtime.size_only);
        assert!(!runtime.overwrite);
        assert_eq!(runtime.progress_every, 42);
        assert_eq!(
            runtime.log,
            Some(PathBuf::from("/tmp/nightindex-compat.log"))
        );
        assert_eq!(runtime.source, PathBuf::from("source"));
        assert_eq!(runtime.destination, PathBuf::from("dest"));
        assert!(
            !runtime
                .unsupported_args
                .iter()
                .any(|item| item == "--ignore-times")
        );
        assert!(
            runtime
                .unsupported_args
                .iter()
                .any(|item| item == "extra positional: unused")
        );
        assert!(
            runtime
                .exclude_prefixes
                .iter()
                .any(|item| item == "tmp/cache")
        );
        assert!(runtime.exclude_prefixes.iter().any(|item| item == "cache"));
        assert!(
            runtime
                .exclude_prefixes
                .iter()
                .any(|item| item == "node_modules")
        );
        assert!(runtime.policy.is_none());

        fs::remove_dir_all(&root).ok();
        Ok(())
    }

    #[test]
    fn parse_compat_copy_flags_size_only_support() -> Result<()> {
        let args = CompatCopyArgs {
            compat_args: vec!["--size-only".into(), "left".into(), "right".into()],
        };

        let runtime = parse_compat_copy_flags(&args, "rclone")?;
        assert!(runtime.size_only);
        assert_eq!(runtime.source, PathBuf::from("left"));
        assert_eq!(runtime.destination, PathBuf::from("right"));
        Ok(())
    }

    #[test]
    fn parse_compat_copy_flags_supports_max_age() -> Result<()> {
        let args = CompatCopyArgs {
            compat_args: vec![
                "--max-age".into(),
                "10m".into(),
                "left".into(),
                "right".into(),
            ],
        };

        let runtime = parse_compat_copy_flags(&args, "rclone")?;
        assert_eq!(runtime.max_age_ns, Some(10 * 60 * 1_000_000_000));
        assert_eq!(runtime.source, PathBuf::from("left"));
        assert_eq!(runtime.destination, PathBuf::from("right"));
        Ok(())
    }

    #[test]
    fn parse_compat_copy_flags_supports_delete_excluded() -> Result<()> {
        let args = CompatCopyArgs {
            compat_args: vec!["--delete-excluded".into(), "left".into(), "right".into()],
        };

        let runtime = parse_compat_copy_flags(&args, "rsync")?;
        assert!(runtime.delete_excluded);
        assert_eq!(runtime.source, PathBuf::from("left"));
        assert_eq!(runtime.destination, PathBuf::from("right"));
        Ok(())
    }

    #[test]
    fn parse_compat_copy_flags_marks_unknown_short_flags() -> Result<()> {
        let args = CompatCopyArgs {
            compat_args: vec!["-nQ".into(), "left".into(), "right".into()],
        };

        let runtime = parse_compat_copy_flags(&args, "rclone")?;
        assert!(runtime.dry_run);
        assert!(runtime.unsupported_args.contains(&"-Q".to_string()));
        assert_eq!(runtime.source, PathBuf::from("left"));
        assert_eq!(runtime.destination, PathBuf::from("right"));
        Ok(())
    }

    #[test]
    fn parse_compat_copy_flags_supports_delete_and_inplace() -> Result<()> {
        let args = CompatCopyArgs {
            compat_args: vec![
                "--stop-on-error".into(),
                "--delete-after".into(),
                "--inplace".into(),
                "source".into(),
                "dest".into(),
            ],
        };

        let runtime = parse_compat_copy_flags(&args, "rsync")?;
        assert!(runtime.stop_on_error);
        assert!(matches!(runtime.delete_mode, Some(DeleteMode::After)));
        assert!(runtime.inplace);
        assert!(runtime.unsupported_args.is_empty());
        assert_eq!(runtime.source, PathBuf::from("source"));
        assert_eq!(runtime.destination, PathBuf::from("dest"));
        Ok(())
    }

    #[test]
    fn parse_compat_copy_flags_records_accepted_link_flags() -> Result<()> {
        let args = CompatCopyArgs {
            compat_args: vec![
                "--copy-links".into(),
                "--copy-unsafe-links".into(),
                "--links".into(),
                "source".into(),
                "dest".into(),
            ],
        };

        let runtime = parse_compat_copy_flags(&args, "rsync")?;
        assert_eq!(
            runtime.accepted_link_flags,
            vec![
                "--copy-links".to_string(),
                "--copy-unsafe-links".to_string(),
                "--links".to_string(),
            ]
        );
        assert!(runtime.unsupported_args.is_empty());
        Ok(())
    }

    #[test]
    fn parse_compat_copy_flags_supports_filter_and_include() -> Result<()> {
        let root = temp_dir("filter_include");
        let include_file = root.join("include.txt");
        let filter_file = root.join("filter.txt");
        fs::write(&include_file, "QCOM/*\nARM64/**\n")?;
        fs::write(&filter_file, "+ QCOM/**\n- QCOM/tmp/*\n")?;

        let args = CompatCopyArgs {
            compat_args: vec![
                "--include".into(),
                "EXTRA/*.bin".into(),
                "--include-from".into(),
                include_file.display().to_string(),
                "--filter".into(),
                "+ ARM64/**".into(),
                "--filter-from".into(),
                filter_file.display().to_string(),
                "source".into(),
                "dest".into(),
            ],
        };

        let runtime = parse_compat_copy_flags(&args, "rsync")?;
        assert!(
            runtime
                .include_patterns
                .iter()
                .any(|item| item.display_value() == "EXTRA/*.bin")
        );
        assert!(
            runtime
                .include_patterns
                .iter()
                .any(|item| item.display_value() == "QCOM/*")
        );
        assert!(
            runtime
                .include_patterns
                .iter()
                .any(|item| item.display_value() == "ARM64/**")
        );
        assert!(
            runtime
                .filter_exclude_patterns
                .iter()
                .any(|item| item.display_value() == "QCOM/tmp/*")
        );
        assert_eq!(runtime.source, PathBuf::from("source"));
        assert_eq!(runtime.destination, PathBuf::from("dest"));
        fs::remove_dir_all(&root).ok();
        Ok(())
    }

    #[test]
    fn parse_compat_copy_flags_supports_common_compat_flags() -> Result<()> {
        let root = temp_dir("compat_common");
        let files_from = root.join("files-from.txt");
        fs::write(&files_from, "keep.bin\nnested/\n#comment\n")?;

        let args = CompatCopyArgs {
            compat_args: vec![
                "--files-from".into(),
                files_from.display().to_string(),
                "--exclude-if-present".into(),
                ".nobackup".into(),
                "--backup".into(),
                "--backup-dir".into(),
                "/tmp/nightindex-backups".into(),
                "--stats".into(),
                "--human-readable".into(),
                "-vv".into(),
                "source/".into(),
                "dest/".into(),
            ],
        };

        let runtime = parse_compat_copy_flags(&args, "rsync")?;
        assert!(runtime.backup_requested);
        assert_eq!(
            runtime.backup_dir,
            Some(PathBuf::from("/tmp/nightindex-backups"))
        );
        assert!(runtime.stats);
        assert!(runtime.human_readable);
        assert_eq!(runtime.verbosity, 2);
        assert!(runtime.source_trailing_slash);
        assert!(runtime.destination_trailing_slash);
        assert_eq!(
            runtime
                .exclude_if_present
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec![".nobackup"]
        );
        assert!(
            runtime
                .files_from_patterns
                .iter()
                .any(|item| item.display_value() == "keep.bin")
        );
        assert!(
            runtime
                .files_from_patterns
                .iter()
                .any(|item| item.display_value() == "nested/")
        );
        fs::remove_dir_all(&root).ok();
        Ok(())
    }

    #[test]
    fn parse_compat_copy_flags_ignores_empty_include_values() -> Result<()> {
        let args = CompatCopyArgs {
            compat_args: vec!["--include=/".into(), "source".into(), "dest".into()],
        };

        let runtime = parse_compat_copy_flags(&args, "rsync")?;
        assert!(runtime.include_patterns.is_empty());
        assert_eq!(runtime.source, PathBuf::from("source"));
        assert_eq!(runtime.destination, PathBuf::from("dest"));
        Ok(())
    }

    #[test]
    fn filter_plan_by_patterns_respects_directory_only_patterns() {
        let plan = CopyPlan {
            mode: "copy-missing".to_string(),
            left_label: "left".to_string(),
            right_label: "right".to_string(),
            left_db: None,
            right_db: None,
            generated_at_ns: 0,
            summary: CopyPlanSummary {
                files_to_copy: 3,
                bytes_to_copy: 12,
                left_files: 3,
                right_files: 0,
            },
            items: vec![
                CopyPlanItem {
                    rel_path: "QCOM".to_string(),
                    file_type: "file".to_string(),
                    size: 1,
                    mtime_ns: 1,
                    fast_hash: None,
                },
                CopyPlanItem {
                    rel_path: "QCOM/readme.md".to_string(),
                    file_type: "file".to_string(),
                    size: 2,
                    mtime_ns: 2,
                    fast_hash: None,
                },
                CopyPlanItem {
                    rel_path: "QCOM/tools/helper.py".to_string(),
                    file_type: "file".to_string(),
                    size: 3,
                    mtime_ns: 3,
                    fast_hash: None,
                },
            ],
        };

        let filtered = filter_plan_by_patterns(
            &plan,
            &[PatternSpec::parse("QCOM/").expect("dir-only pattern")],
            &[],
        );

        assert_eq!(filtered.summary.files_to_copy, 2);
        assert_eq!(filtered.summary.bytes_to_copy, 5);
        assert_eq!(filtered.items.len(), 2);
        assert!(
            filtered
                .items
                .iter()
                .any(|item| item.rel_path == "QCOM/readme.md")
        );
        assert!(
            filtered
                .items
                .iter()
                .any(|item| item.rel_path == "QCOM/tools/helper.py")
        );
        assert!(!filtered.items.iter().any(|item| item.rel_path == "QCOM"));
    }

    #[test]
    fn filter_plan_by_files_from_keeps_listed_paths() {
        let plan = CopyPlan {
            mode: "copy-missing".to_string(),
            left_label: "left".to_string(),
            right_label: "right".to_string(),
            left_db: None,
            right_db: None,
            generated_at_ns: 0,
            summary: CopyPlanSummary {
                files_to_copy: 3,
                bytes_to_copy: 12,
                left_files: 3,
                right_files: 0,
            },
            items: vec![
                CopyPlanItem {
                    rel_path: "keep.bin".to_string(),
                    file_type: "file".to_string(),
                    size: 2,
                    mtime_ns: 1,
                    fast_hash: None,
                },
                CopyPlanItem {
                    rel_path: "nested/item.txt".to_string(),
                    file_type: "file".to_string(),
                    size: 4,
                    mtime_ns: 2,
                    fast_hash: None,
                },
                CopyPlanItem {
                    rel_path: "other.bin".to_string(),
                    file_type: "file".to_string(),
                    size: 6,
                    mtime_ns: 3,
                    fast_hash: None,
                },
            ],
        };

        let filtered = filter_plan_by_files_from(
            &plan,
            &[
                PatternSpec::parse("keep.bin").expect("file pattern"),
                PatternSpec::parse("nested/").expect("dir pattern"),
            ],
        );

        assert_eq!(filtered.summary.files_to_copy, 2);
        assert_eq!(filtered.summary.bytes_to_copy, 6);
        assert_eq!(filtered.items.len(), 2);
        assert!(
            filtered
                .items
                .iter()
                .any(|item| item.rel_path == "keep.bin")
        );
        assert!(
            filtered
                .items
                .iter()
                .any(|item| item.rel_path == "nested/item.txt")
        );
        assert!(
            !filtered
                .items
                .iter()
                .any(|item| item.rel_path == "other.bin")
        );
    }

    #[test]
    fn filter_plan_by_max_age_keeps_only_recent_items() {
        let plan = CopyPlan {
            mode: "copy-missing".to_string(),
            left_label: "left".to_string(),
            right_label: "right".to_string(),
            left_db: None,
            right_db: None,
            generated_at_ns: 0,
            summary: CopyPlanSummary {
                files_to_copy: 3,
                bytes_to_copy: 12,
                left_files: 3,
                right_files: 0,
            },
            items: vec![
                CopyPlanItem {
                    rel_path: "old.bin".to_string(),
                    file_type: "file".to_string(),
                    size: 2,
                    mtime_ns: 1_000,
                    fast_hash: None,
                },
                CopyPlanItem {
                    rel_path: "fresh.bin".to_string(),
                    file_type: "file".to_string(),
                    size: 4,
                    mtime_ns: 950_000_000,
                    fast_hash: None,
                },
                CopyPlanItem {
                    rel_path: "link".to_string(),
                    file_type: "symlink".to_string(),
                    size: 6,
                    mtime_ns: 980_000_000,
                    fast_hash: None,
                },
            ],
        };

        let filtered = filter_plan_by_max_age(&plan, 100_000_000, 1_000_000_000);

        assert_eq!(filtered.summary.files_to_copy, 2);
        assert_eq!(filtered.summary.bytes_to_copy, 10);
        assert_eq!(filtered.items.len(), 2);
        assert!(
            filtered
                .items
                .iter()
                .any(|item| item.rel_path == "fresh.bin")
        );
        assert!(filtered.items.iter().any(|item| item.rel_path == "link"));
        assert!(!filtered.items.iter().any(|item| item.rel_path == "old.bin"));
    }

    #[test]
    fn delete_pass_targets_destination_only_files() -> Result<()> {
        let root = temp_dir("delete_pass");
        let source_root = root.join("source");
        let destination_root = root.join("destination");
        fs::create_dir_all(&source_root)?;
        fs::create_dir_all(&destination_root)?;

        fs::write(source_root.join("keep.bin"), b"keep")?;
        fs::write(destination_root.join("keep.bin"), b"keep")?;
        fs::write(destination_root.join("orphan.bin"), b"orphan")?;

        let source_db = root.join("source.sqlite");
        let destination_db = root.join("destination.sqlite");

        scan_command(ScanArgs {
            db: source_db.clone(),
            label: "left".to_string(),
            root: source_root,
            exclude_prefixes: Vec::new(),
            exclude_if_present: Vec::new(),
            policy: None,
            hash: false,
        })?;
        scan_command(ScanArgs {
            db: destination_db.clone(),
            label: "right".to_string(),
            root: destination_root.clone(),
            exclude_prefixes: Vec::new(),
            exclude_if_present: Vec::new(),
            policy: None,
            hash: false,
        })?;

        let summary = run_delete_pass(
            &source_db,
            &destination_db,
            "left",
            "right",
            DeleteRunArgs {
                destination_root: destination_root.clone(),
                backup_dir: None,
                dry_run: true,
                stop_on_error: false,
                log: None,
                progress_every: 1,
                delete_excluded: false,
            },
            None,
        )?;

        assert_eq!(summary.deleted_files, 1);
        assert_eq!(summary.deleted_bytes, 6);
        assert!(destination_root.join("orphan.bin").exists());

        fs::remove_dir_all(&root).ok();
        Ok(())
    }

    #[test]
    fn delete_pass_can_remove_excluded_destination_files() -> Result<()> {
        let root = temp_dir("delete_excluded");
        let source_root = root.join("source");
        let destination_root = root.join("destination");
        fs::create_dir_all(&source_root)?;
        fs::create_dir_all(&destination_root)?;

        fs::write(source_root.join("keep.bin"), b"keep")?;
        fs::create_dir_all(source_root.join("skip"))?;
        fs::write(source_root.join("skip/inner.bin"), b"skip")?;
        fs::write(destination_root.join("keep.bin"), b"keep")?;
        fs::create_dir_all(destination_root.join("skip"))?;
        fs::write(destination_root.join("skip/inner.bin"), b"skip")?;
        fs::write(destination_root.join("orphan.bin"), b"orphan")?;

        let source_db = root.join("source.sqlite");
        let destination_db = root.join("destination.sqlite");

        scan_command(ScanArgs {
            db: source_db.clone(),
            label: "left".to_string(),
            root: source_root,
            exclude_prefixes: Vec::new(),
            exclude_if_present: Vec::new(),
            policy: None,
            hash: false,
        })?;
        scan_command(ScanArgs {
            db: destination_db.clone(),
            label: "right".to_string(),
            root: destination_root.clone(),
            exclude_prefixes: Vec::new(),
            exclude_if_present: Vec::new(),
            policy: None,
            hash: false,
        })?;

        let policy = ExcludePolicy {
            directory_prefixes: vec!["skip".to_string()],
            folder_name_additions: Vec::new(),
            subtree_overrides: HashMap::new(),
            enabled: true,
        };

        let summary = run_delete_pass(
            &source_db,
            &destination_db,
            "left",
            "right",
            DeleteRunArgs {
                destination_root: destination_root.clone(),
                backup_dir: None,
                dry_run: false,
                stop_on_error: false,
                log: None,
                progress_every: 1,
                delete_excluded: true,
            },
            Some(&policy),
        )?;

        assert_eq!(summary.deleted_files, 2);
        assert_eq!(summary.deleted_bytes, 10);
        assert!(!destination_root.join("skip/inner.bin").exists());
        assert!(!destination_root.join("orphan.bin").exists());
        assert!(destination_root.join("keep.bin").exists());

        fs::remove_dir_all(&root).ok();
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn scan_command_records_symlinks() -> Result<()> {
        let root = temp_dir("scan_symlink");
        let source_root = root.join("source");
        fs::create_dir_all(&source_root)?;

        fs::write(source_root.join("payload.txt"), b"payload")?;
        std::os::unix::fs::symlink("payload.txt", source_root.join("payload.link"))?;

        let db = root.join("scan.sqlite");
        scan_command(ScanArgs {
            db: db.clone(),
            label: "left".to_string(),
            root: source_root,
            exclude_prefixes: Vec::new(),
            exclude_if_present: Vec::new(),
            policy: None,
            hash: false,
        })?;

        let conn = open_readonly_db(&db)?;
        let rows = load_label(&conn, "left")?;
        assert_eq!(rows.len(), 2);
        let file_row = rows
            .iter()
            .find(|row| row.rel_path == "payload.txt")
            .expect("file row");
        assert_eq!(file_row.file_type, "file");
        let link_row = rows
            .iter()
            .find(|row| row.rel_path == "payload.link")
            .expect("symlink row");
        assert_eq!(link_row.file_type, "symlink");
        assert_eq!(link_row.size, 0);
        assert!(link_row.mtime_ns > 0);
        assert_eq!(link_row.fast_hash.as_deref(), Some("payload.txt"));

        fs::remove_dir_all(&root).ok();
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn copy_plan_preserves_symlinks() -> Result<()> {
        let root = temp_dir("copy_symlink");
        let source_root = root.join("source");
        let destination_root = root.join("destination");
        fs::create_dir_all(&source_root)?;
        fs::create_dir_all(&destination_root)?;

        fs::write(source_root.join("payload.txt"), b"payload")?;
        std::os::unix::fs::symlink("payload.txt", source_root.join("payload.link"))?;

        let source_db = root.join("source.sqlite");
        let destination_db = root.join("destination.sqlite");

        scan_command(ScanArgs {
            db: source_db.clone(),
            label: "left".to_string(),
            root: source_root.clone(),
            exclude_prefixes: Vec::new(),
            exclude_if_present: Vec::new(),
            policy: None,
            hash: false,
        })?;
        scan_command(ScanArgs {
            db: destination_db.clone(),
            label: "right".to_string(),
            root: destination_root.clone(),
            exclude_prefixes: Vec::new(),
            exclude_if_present: Vec::new(),
            policy: None,
            hash: false,
        })?;

        let plan = build_copy_missing_plan(&source_db, &destination_db, "left", "right", None)?;
        assert!(
            plan.items
                .iter()
                .any(|item| item.rel_path == "payload.link" && item.file_type == "symlink")
        );

        let summary = execute_copy_missing_with_plan(
            &plan,
            CopyRunArgs {
                source_root: source_root.clone(),
                destination_root: destination_root.clone(),
                backup_dir: None,
                overwrite: false,
                dry_run: false,
                stop_on_error: false,
                log: None,
                progress_every: 1,
                size_only: false,
                hash: false,
                copy_links_as_files: false,
            },
            None,
        )?;

        assert_eq!(summary.copied_files, 2);
        let link_target = std::fs::read_link(destination_root.join("payload.link"))?;
        assert_eq!(link_target, PathBuf::from("payload.txt"));
        assert!(destination_root.join("payload.txt").exists());

        fs::remove_dir_all(&root).ok();
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn copy_links_mode_copies_symlink_target_contents() -> Result<()> {
        let root = temp_dir("copy_links_mode");
        let source_root = root.join("source");
        let destination_root = root.join("destination");
        fs::create_dir_all(&source_root)?;
        fs::create_dir_all(&destination_root)?;

        fs::write(source_root.join("payload.txt"), b"payload")?;
        std::os::unix::fs::symlink("payload.txt", source_root.join("payload.link"))?;

        let source_db = root.join("source.sqlite");
        let destination_db = root.join("destination.sqlite");

        scan_command(ScanArgs {
            db: source_db.clone(),
            label: "left".to_string(),
            root: source_root.clone(),
            exclude_prefixes: Vec::new(),
            exclude_if_present: Vec::new(),
            policy: None,
            hash: false,
        })?;
        scan_command(ScanArgs {
            db: destination_db.clone(),
            label: "right".to_string(),
            root: destination_root.clone(),
            exclude_prefixes: Vec::new(),
            exclude_if_present: Vec::new(),
            policy: None,
            hash: false,
        })?;

        let plan = build_copy_missing_plan(&source_db, &destination_db, "left", "right", None)?;
        let summary = execute_copy_missing_with_plan(
            &plan,
            CopyRunArgs {
                source_root: source_root.clone(),
                destination_root: destination_root.clone(),
                backup_dir: None,
                overwrite: false,
                dry_run: false,
                stop_on_error: false,
                log: None,
                progress_every: 1,
                size_only: false,
                hash: false,
                copy_links_as_files: true,
            },
            None,
        )?;

        assert_eq!(summary.copied_files, 2);
        let payload_meta = std::fs::metadata(destination_root.join("payload.link"))?;
        assert!(payload_meta.is_file());
        assert!(
            !std::fs::symlink_metadata(destination_root.join("payload.link"))?
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read(destination_root.join("payload.link"))?,
            b"payload"
        );

        fs::remove_dir_all(&root).ok();
        Ok(())
    }

    #[test]
    fn filter_plan_by_patterns_applies_allowlist_and_blocklist() {
        let plan = CopyPlan {
            mode: "copy-missing".to_string(),
            left_label: "left".to_string(),
            right_label: "right".to_string(),
            left_db: None,
            right_db: None,
            generated_at_ns: 0,
            summary: CopyPlanSummary {
                files_to_copy: 3,
                bytes_to_copy: 12,
                left_files: 3,
                right_files: 0,
            },
            items: vec![
                CopyPlanItem {
                    rel_path: "QCOM/keep.bin".to_string(),
                    file_type: "file".to_string(),
                    size: 2,
                    mtime_ns: 1,
                    fast_hash: None,
                },
                CopyPlanItem {
                    rel_path: "QCOM/tmp/drop.bin".to_string(),
                    file_type: "file".to_string(),
                    size: 4,
                    mtime_ns: 2,
                    fast_hash: None,
                },
                CopyPlanItem {
                    rel_path: "ARM64/skip.bin".to_string(),
                    file_type: "file".to_string(),
                    size: 6,
                    mtime_ns: 3,
                    fast_hash: None,
                },
            ],
        };

        let filtered = filter_plan_by_patterns(
            &plan,
            &[
                PatternSpec::parse("QCOM/**").expect("pattern"),
                PatternSpec::parse("ARM64/*.bin").expect("pattern"),
            ],
            &[PatternSpec::parse("QCOM/tmp/*").expect("pattern")],
        );

        assert_eq!(filtered.summary.files_to_copy, 2);
        assert_eq!(filtered.summary.bytes_to_copy, 8);
        assert_eq!(filtered.items.len(), 2);
        assert!(
            filtered
                .items
                .iter()
                .any(|item| item.rel_path == "QCOM/keep.bin")
        );
        assert!(
            filtered
                .items
                .iter()
                .any(|item| item.rel_path == "ARM64/skip.bin")
        );
        assert!(
            !filtered
                .items
                .iter()
                .any(|item| item.rel_path == "QCOM/tmp/drop.bin")
        );
    }

    #[test]
    fn build_dossier_matches_returns_top_k_matches() {
        let left_rows = vec![
            FileRecord {
                rel_path: "alpha/readme.txt".to_string(),
                file_type: "file".to_string(),
                size: 101,
                mtime_ns: 1,
                fast_hash: Some("hash-readme-left".to_string()),
            },
            FileRecord {
                rel_path: "alpha/app.bin".to_string(),
                file_type: "file".to_string(),
                size: 202,
                mtime_ns: 2,
                fast_hash: Some("hash-bin-left".to_string()),
            },
            FileRecord {
                rel_path: "alpha/notes.log".to_string(),
                file_type: "file".to_string(),
                size: 303,
                mtime_ns: 3,
                fast_hash: Some("hash-notes-left".to_string()),
            },
        ];
        let right_rows = vec![
            FileRecord {
                rel_path: "beta/readme.txt".to_string(),
                file_type: "file".to_string(),
                size: 111,
                mtime_ns: 10,
                fast_hash: Some("hash-readme-left".to_string()),
            },
            FileRecord {
                rel_path: "beta/app.bin".to_string(),
                file_type: "file".to_string(),
                size: 222,
                mtime_ns: 11,
                fast_hash: Some("hash-bin-left".to_string()),
            },
            FileRecord {
                rel_path: "omega/readme.txt".to_string(),
                file_type: "file".to_string(),
                size: 333,
                mtime_ns: 12,
                fast_hash: Some("hash-readme-left".to_string()),
            },
            FileRecord {
                rel_path: "omega/other.tmp".to_string(),
                file_type: "file".to_string(),
                size: 444,
                mtime_ns: 13,
                fast_hash: Some("hash-other".to_string()),
            },
        ];

        let left_signatures = build_folder_signatures(&left_rows);
        let right_signatures = build_folder_signatures(&right_rows);
        let matches = build_dossier_matches(&left_signatures, &right_signatures, 1);

        assert_eq!(left_signatures.len(), 1);
        assert_eq!(right_signatures.len(), 2);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].left_folder, "alpha");
        assert_eq!(matches[0].right_folder, "beta");
        assert!((matches[0].overlap_weight > 0.0));
        assert_eq!(matches[0].shared_rel_file_count, 2);
        assert_eq!(matches[0].shared_exact_file_name_count, 2);
        assert_eq!(matches[0].shared_normalized_file_name_count, 2);
        assert_eq!(matches[0].shared_file_stem_count, 2);
        assert_eq!(matches[0].shared_file_ext_count, 2);
        assert_eq!(matches[0].shared_ext_stem_count, 3);
        assert_eq!(matches[0].shared_hash_count, 2);
        assert_eq!(matches[0].shared_folder_token_count, 0);
        assert_eq!(matches[0].shared_normalized_parent_folder_count, 0);

        let all = build_dossier_matches(&left_signatures, &right_signatures, 5);
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn dossier_matching_reports_hash_signal_for_divergent_file_names() {
        let left_rows = vec![
            FileRecord {
                rel_path: "alpha/raw-seed-01.txt".to_string(),
                file_type: "file".to_string(),
                size: 101,
                mtime_ns: 1,
                fast_hash: Some("h1".to_string()),
            },
            FileRecord {
                rel_path: "alpha/results-final.bin".to_string(),
                file_type: "file".to_string(),
                size: 202,
                mtime_ns: 2,
                fast_hash: Some("h2".to_string()),
            },
        ];
        let right_rows = vec![
            FileRecord {
                rel_path: "renamed/seed-block.txt".to_string(),
                file_type: "file".to_string(),
                size: 111,
                mtime_ns: 10,
                fast_hash: Some("h1".to_string()),
            },
            FileRecord {
                rel_path: "renamed/chosen-output.bin".to_string(),
                file_type: "file".to_string(),
                size: 112,
                mtime_ns: 11,
                fast_hash: Some("h2".to_string()),
            },
        ];

        let left_signatures = build_folder_signatures(&left_rows);
        let right_signatures = build_folder_signatures(&right_rows);
        let matches = build_dossier_matches(&left_signatures, &right_signatures, 1);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].left_folder, "alpha");
        assert_eq!(matches[0].right_folder, "renamed");
        assert!(matches[0].overlap_weight > 0.0);
        assert_eq!(matches[0].shared_exact_file_name_count, 0);
        assert_eq!(matches[0].shared_normalized_file_name_count, 0);
        assert_eq!(matches[0].shared_file_stem_count, 0);
        assert_eq!(matches[0].shared_ext_stem_count, 0);
        assert_eq!(matches[0].shared_hash_count, 2);
        assert_eq!(matches[0].shared_rel_file_count, 0);
        assert_eq!(matches[0].confidence_tier, DossierConfidenceTier::Possible);
    }

    #[test]
    fn dossier_matching_shows_weak_extension_only_for_mismatched_patterns() {
        let left_rows = vec![
            FileRecord {
                rel_path: "source-pack/logs-alpha.txt".to_string(),
                file_type: "file".to_string(),
                size: 101,
                mtime_ns: 1,
                fast_hash: Some("left-h1".to_string()),
            },
            FileRecord {
                rel_path: "source-pack/chunk-beta.bin".to_string(),
                file_type: "file".to_string(),
                size: 202,
                mtime_ns: 2,
                fast_hash: Some("left-h2".to_string()),
            },
        ];
        let right_rows = vec![
            FileRecord {
                rel_path: "payload-v2/alpha-log.txt".to_string(),
                file_type: "file".to_string(),
                size: 111,
                mtime_ns: 10,
                fast_hash: Some("right-h3".to_string()),
            },
            FileRecord {
                rel_path: "payload-v2/block-zed.bin".to_string(),
                file_type: "file".to_string(),
                size: 222,
                mtime_ns: 11,
                fast_hash: Some("right-h4".to_string()),
            },
        ];

        let left_signatures = build_folder_signatures(&left_rows);
        let right_signatures = build_folder_signatures(&right_rows);
        let matches = build_dossier_matches(&left_signatures, &right_signatures, 1);

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].left_folder, "source-pack");
        assert_eq!(matches[0].right_folder, "payload-v2");
        assert!(matches[0].overlap_weight > 0.0);
        assert_eq!(matches[0].shared_exact_file_name_count, 0);
        assert_eq!(matches[0].shared_normalized_file_name_count, 0);
        assert_eq!(matches[0].shared_file_stem_count, 0);
        assert_eq!(matches[0].shared_file_ext_count, 2);
        assert_eq!(matches[0].shared_hash_count, 0);
        assert_eq!(matches[0].confidence_tier, DossierConfidenceTier::Manual);
    }

    #[test]
    fn dossier_matching_prefers_language_signal_when_names_drift() {
        let left_rows = vec![
            FileRecord {
                rel_path: "readme-source/readme-a.txt".to_string(),
                file_type: "file".to_string(),
                size: 128,
                mtime_ns: 1,
                fast_hash: None,
            },
            FileRecord {
                rel_path: "readme-source/notes-a.txt".to_string(),
                file_type: "file".to_string(),
                size: 256,
                mtime_ns: 2,
                fast_hash: None,
            },
        ];
        let right_rows = vec![
            FileRecord {
                rel_path: "readme-copy/readme-b.txt".to_string(),
                file_type: "file".to_string(),
                size: 136,
                mtime_ns: 10,
                fast_hash: None,
            },
            FileRecord {
                rel_path: "readme-copy/notes-b.txt".to_string(),
                file_type: "file".to_string(),
                size: 274,
                mtime_ns: 11,
                fast_hash: None,
            },
            FileRecord {
                rel_path: "candidate-plain/doc-a.txt".to_string(),
                file_type: "file".to_string(),
                size: 136,
                mtime_ns: 12,
                fast_hash: None,
            },
            FileRecord {
                rel_path: "candidate-plain/doc-b.txt".to_string(),
                file_type: "file".to_string(),
                size: 274,
                mtime_ns: 13,
                fast_hash: None,
            },
        ];

        let left_signatures = build_folder_signatures_with_profiles(&left_rows, &HashMap::new());
        let right_signatures = build_folder_signatures_with_profiles(&right_rows, &HashMap::new());
        let matches = build_dossier_matches(&left_signatures, &right_signatures, 2);

        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].left_folder, "readme-source");
        assert_eq!(matches[0].right_folder, "readme-copy");
        assert_eq!(matches[0].shared_language_count, 1);
        assert_eq!(matches[0].shared_size_class_count, 2);
        assert_eq!(matches[1].right_folder, "candidate-plain");
        assert_eq!(matches[1].shared_language_count, 0);
        assert!(matches[0].overlap_ratio > matches[1].overlap_ratio);
    }

    #[test]
    fn dossier_matching_prefers_size_bucket_signal_when_names_drift() {
        let left_rows = vec![
            FileRecord {
                rel_path: "size-drift/alpha.txt".to_string(),
                file_type: "file".to_string(),
                size: 128,
                mtime_ns: 1,
                fast_hash: None,
            },
            FileRecord {
                rel_path: "size-drift/beta.txt".to_string(),
                file_type: "file".to_string(),
                size: 256,
                mtime_ns: 2,
                fast_hash: None,
            },
        ];
        let right_rows = vec![
            FileRecord {
                rel_path: "size-match/branch-one.txt".to_string(),
                file_type: "file".to_string(),
                size: 80,
                mtime_ns: 10,
                fast_hash: None,
            },
            FileRecord {
                rel_path: "size-match/branch-two.txt".to_string(),
                file_type: "file".to_string(),
                size: 200,
                mtime_ns: 11,
                fast_hash: None,
            },
            FileRecord {
                rel_path: "size-mismatch/branch-one.txt".to_string(),
                file_type: "file".to_string(),
                size: 80,
                mtime_ns: 12,
                fast_hash: None,
            },
            FileRecord {
                rel_path: "size-mismatch/branch-two.txt".to_string(),
                file_type: "file".to_string(),
                size: 20 * 1024 * 1024,
                mtime_ns: 13,
                fast_hash: None,
            },
        ];

        let left_signatures = build_folder_signatures_with_profiles(&left_rows, &HashMap::new());
        let right_signatures = build_folder_signatures_with_profiles(&right_rows, &HashMap::new());
        let matches = build_dossier_matches(&left_signatures, &right_signatures, 2);

        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].left_folder, "size-drift");
        assert_eq!(matches[0].right_folder, "size-match");
        assert_eq!(matches[0].shared_size_class_count, 2);
        assert_eq!(matches[1].right_folder, "size-mismatch");
        assert_eq!(matches[1].shared_size_class_count, 2);
        assert!(matches[0].overlap_ratio > matches[1].overlap_ratio);
    }

    #[test]
    fn build_dossier_csv_emits_signal_count_columns() {
        let rows = vec![DossierMatch {
            left_folder: "alpha".to_string(),
            right_folder: "beta".to_string(),
            overlap_weight: 1.25,
            left_weight: 2.0,
            right_weight: 2.5,
            overlap_ratio: 0.357,
            shared_rel_file_count: 1,
            shared_exact_file_name_count: 2,
            shared_normalized_file_name_count: 1,
            shared_file_stem_count: 1,
            shared_file_ext_count: 1,
            shared_ext_stem_count: 1,
            shared_hash_count: 1,
            shared_folder_token_count: 0,
            shared_normalized_parent_folder_count: 1,
            shared_binaryity_count: 2,
            shared_archive_family_count: 0,
            shared_language_count: 0,
            shared_size_class_count: 0,
            confidence_tier: DossierConfidenceTier::Possible,
        }];

        let csv = build_dossier_csv(&rows);
        assert!(csv.starts_with(
            "left_folder,right_folder,overlap_weight,left_weight,right_weight,overlap_ratio,shared_rel_file_count,shared_exact_file_name_count,shared_normalized_file_name_count,shared_file_stem_count,shared_file_ext_count,shared_ext_stem_count,shared_hash_count,shared_folder_token_count,shared_normalized_parent_folder_count,shared_binaryity_count,shared_archive_family_count,shared_language_count,shared_size_class_count,confidence_tier\n",
        ));
        assert!(csv.contains(
            "alpha,beta,1.2500,2.0000,2.5000,0.357000,1,2,1,1,1,1,1,0,1,2,0,0,0,possible"
        ));
    }

    #[test]
    fn build_dossier_actions_csv_emits_ranked_actions() {
        let rows = vec![
            DossierMatch {
                left_folder: "alpha".to_string(),
                right_folder: "beta".to_string(),
                overlap_weight: 1.25,
                left_weight: 2.0,
                right_weight: 2.5,
                overlap_ratio: 0.357,
                shared_rel_file_count: 1,
                shared_exact_file_name_count: 2,
                shared_normalized_file_name_count: 1,
                shared_file_stem_count: 1,
                shared_file_ext_count: 1,
                shared_ext_stem_count: 1,
                shared_hash_count: 1,
                shared_folder_token_count: 0,
                shared_normalized_parent_folder_count: 1,
                shared_binaryity_count: 2,
                shared_archive_family_count: 0,
                shared_language_count: 0,
                shared_size_class_count: 0,
                confidence_tier: DossierConfidenceTier::Possible,
            },
            DossierMatch {
                left_folder: "alpha".to_string(),
                right_folder: "gamma".to_string(),
                overlap_weight: 0.75,
                left_weight: 2.0,
                right_weight: 2.5,
                overlap_ratio: 0.157,
                shared_rel_file_count: 0,
                shared_exact_file_name_count: 0,
                shared_normalized_file_name_count: 0,
                shared_file_stem_count: 1,
                shared_file_ext_count: 1,
                shared_ext_stem_count: 0,
                shared_hash_count: 0,
                shared_folder_token_count: 1,
                shared_normalized_parent_folder_count: 0,
                shared_binaryity_count: 1,
                shared_archive_family_count: 0,
                shared_language_count: 0,
                shared_size_class_count: 1,
                confidence_tier: DossierConfidenceTier::Manual,
            },
        ];

        let csv = build_dossier_actions_csv(&rows);
        assert!(csv.starts_with(
            "left_folder,rank,right_folder,confidence_tier,next_action,overlap_ratio,shared_hash_count,shared_normalized_file_name_count,shared_rel_file_count\n"
        ));
        assert!(csv.contains("alpha,1,beta,possible,review before applying,0.357000,1,1,1"));
        assert!(csv.contains("alpha,2,gamma,manual,manual inspection required,0.157000,0,0,0"));
    }

    #[test]
    fn keep_top_candidate_per_left_keeps_first_match() {
        let rows = vec![
            DossierMatch {
                left_folder: "alpha".to_string(),
                right_folder: "beta".to_string(),
                overlap_weight: 1.25,
                left_weight: 2.0,
                right_weight: 2.5,
                overlap_ratio: 0.357,
                shared_rel_file_count: 1,
                shared_exact_file_name_count: 2,
                shared_normalized_file_name_count: 1,
                shared_file_stem_count: 1,
                shared_file_ext_count: 1,
                shared_ext_stem_count: 1,
                shared_hash_count: 1,
                shared_folder_token_count: 0,
                shared_normalized_parent_folder_count: 1,
                shared_binaryity_count: 2,
                shared_archive_family_count: 0,
                shared_language_count: 0,
                shared_size_class_count: 0,
                confidence_tier: DossierConfidenceTier::Possible,
            },
            DossierMatch {
                left_folder: "alpha".to_string(),
                right_folder: "gamma".to_string(),
                overlap_weight: 0.75,
                left_weight: 2.0,
                right_weight: 2.5,
                overlap_ratio: 0.157,
                shared_rel_file_count: 0,
                shared_exact_file_name_count: 0,
                shared_normalized_file_name_count: 0,
                shared_file_stem_count: 1,
                shared_file_ext_count: 1,
                shared_ext_stem_count: 0,
                shared_hash_count: 0,
                shared_folder_token_count: 1,
                shared_normalized_parent_folder_count: 0,
                shared_binaryity_count: 1,
                shared_archive_family_count: 0,
                shared_language_count: 0,
                shared_size_class_count: 1,
                confidence_tier: DossierConfidenceTier::Manual,
            },
        ];
        let kept = keep_top_candidate_per_left(&rows);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].left_folder, "alpha");
        assert_eq!(kept[0].right_folder, "beta");
    }

    fn make_confidence_match(
        overlap_ratio: f64,
        shared_normalized_file_name_count: usize,
        shared_hash_count: usize,
        shared_normalized_parent_folder_count: usize,
        shared_file_ext_count: usize,
        shared_ext_stem_count: usize,
        shared_rel_file_count: usize,
    ) -> DossierMatch {
        DossierMatch {
            left_folder: "left".to_string(),
            right_folder: "right".to_string(),
            overlap_weight: 1.0,
            left_weight: 2.0,
            right_weight: 2.0,
            overlap_ratio,
            shared_rel_file_count,
            shared_exact_file_name_count: 0,
            shared_normalized_file_name_count,
            shared_file_stem_count: 0,
            shared_file_ext_count,
            shared_ext_stem_count,
            shared_hash_count,
            shared_folder_token_count: 0,
            shared_normalized_parent_folder_count,
            shared_binaryity_count: 0,
            shared_archive_family_count: 0,
            shared_language_count: 0,
            shared_size_class_count: 0,
            confidence_tier: DossierConfidenceTier::Manual,
        }
    }

    #[test]
    fn dossier_confidence_tiers_are_bucketed_by_signal_strength() {
        let identical = make_confidence_match(0.92, 1, 2, 1, 0, 0, 1);
        assert_eq!(
            dossier_confidence_tier(&identical),
            DossierConfidenceTier::Identical
        );

        let similar = make_confidence_match(0.65, 1, 1, 1, 0, 0, 0);
        assert_eq!(
            dossier_confidence_tier(&similar),
            DossierConfidenceTier::Similar
        );

        let possible = make_confidence_match(0.30, 0, 0, 0, 1, 0, 0);
        assert_eq!(
            dossier_confidence_tier(&possible),
            DossierConfidenceTier::Possible
        );

        let manual = make_confidence_match(0.10, 0, 0, 0, 0, 0, 0);
        assert_eq!(
            dossier_confidence_tier(&manual),
            DossierConfidenceTier::Manual
        );
    }

    #[test]
    fn dossier_confidence_tier_maps_to_actions() {
        assert_eq!(
            DossierConfidenceTier::Identical.action(),
            DossierAction::Apply
        );
        assert_eq!(
            DossierConfidenceTier::Similar.action(),
            DossierAction::Review
        );
        assert_eq!(
            DossierConfidenceTier::Possible.action(),
            DossierAction::Review
        );
        assert_eq!(
            DossierConfidenceTier::Manual.action(),
            DossierAction::Manual
        );
    }

    #[test]
    fn logs_command_parses_progress_and_summary_lines() -> Result<()> {
        let root = temp_dir("logs_command_parse");
        fs::create_dir_all(&root)?;
        let path = root.join("copy.ndjson");
        fs::write(
            &path,
            concat!(
                "{\"event\":\"copy_progress\",\"planned_files\":10,\"planned_bytes\":1000,\"completed_files\":3,\"copied_bytes\":250,\"failed_files\":1}\n",
                "{\"schema_version\":2,\"rel_path\":\"x\",\"action\":\"fail\",\"bytes\":0}\n",
                "{\"event\":\"copy_summary\",\"planned_files\":10,\"planned_bytes\":1000,\"copied_files\":7,\"copied_bytes\":900,\"failed_files\":2}\n"
            ),
        )?;
        logs_command(LogsArgs {
            file: path,
            tail: 200,
            failures_only: false,
            top_errors: 5,
            retry_jsonl_out: None,
        })?;
        fs::remove_dir_all(root).ok();
        Ok(())
    }

    #[test]
    fn resume_recorder_persists_session_and_item_states() -> Result<()> {
        let root = temp_dir("resume_recorder");
        let db = root.join("resume.sqlite");
        let src = root.join("src");
        let dst = root.join("dst");
        fs::create_dir_all(&src)?;
        fs::create_dir_all(&dst)?;

        let plan = CopyPlan {
            mode: "copy-missing".to_string(),
            left_label: "left".to_string(),
            right_label: "right".to_string(),
            left_db: Some(db.display().to_string()),
            right_db: None,
            generated_at_ns: 0,
            summary: CopyPlanSummary {
                files_to_copy: 1,
                bytes_to_copy: 100,
                left_files: 1,
                right_files: 0,
            },
            items: vec![CopyPlanItem {
                rel_path: "a.bin".to_string(),
                file_type: "file".to_string(),
                size: 100,
                mtime_ns: 0,
                fast_hash: None,
            }],
        };
        let args = CopyRunArgs {
            source_root: src,
            destination_root: dst,
            backup_dir: None,
            overwrite: false,
            dry_run: false,
            stop_on_error: false,
            log: None,
            progress_every: 1000,
            size_only: false,
            hash: false,
            copy_links_as_files: false,
        };

        let recorder = ResumeRecorder::start(&plan, &args, 1)?.expect("recorder");
        recorder.mark_status("a.bin", "copying", 0, None, true)?;
        recorder.mark_status("a.bin", "done", 100, None, false)?;
        recorder.finish(1, 0)?;

        let conn = open_readonly_db(&db)?;
        let pending = load_resume_pending_items(&conn, &recorder.session_id, false, None)?;
        assert!(pending.is_empty());
        let status: String = conn.query_row(
            "SELECT status FROM copy_resume_items WHERE session_id = ?1 AND rel_path = 'a.bin'",
            params![&recorder.session_id],
            |row| row.get(0),
        )?;
        assert_eq!(status, "done");

        fs::remove_dir_all(root).ok();
        Ok(())
    }

    #[test]
    fn build_resume_copy_plan_uses_latest_session() -> Result<()> {
        let root = temp_dir("resume_plan");
        let db = root.join("resume.sqlite");
        let src = root.join("src");
        let dst = root.join("dst");
        fs::create_dir_all(&src)?;
        fs::create_dir_all(&dst)?;
        fs::write(src.join("a.bin"), b"abc")?;

        let plan = CopyPlan {
            mode: "copy-missing".to_string(),
            left_label: "left".to_string(),
            right_label: "right".to_string(),
            left_db: Some(db.display().to_string()),
            right_db: None,
            generated_at_ns: 0,
            summary: CopyPlanSummary {
                files_to_copy: 1,
                bytes_to_copy: 3,
                left_files: 1,
                right_files: 0,
            },
            items: vec![CopyPlanItem {
                rel_path: "a.bin".to_string(),
                file_type: "file".to_string(),
                size: 3,
                mtime_ns: 0,
                fast_hash: None,
            }],
        };
        let args = CopyRunArgs {
            source_root: src.clone(),
            destination_root: dst.clone(),
            backup_dir: None,
            overwrite: false,
            dry_run: true,
            stop_on_error: false,
            log: None,
            progress_every: 1000,
            size_only: false,
            hash: false,
            copy_links_as_files: false,
        };

        let recorder = ResumeRecorder::start(&plan, &args, 1)?.expect("recorder");
        let conn = open_db(&db)?;
        conn.execute(
            "INSERT OR REPLACE INTO files(label, rel_path, file_type, size, mtime_ns, fast_hash, scanned_at) VALUES('left','a.bin','file',3,0,NULL,0)",
            params![],
        )?;
        recorder.mark_status("a.bin", "failed", 0, Some("x"), true)?;
        let resume_plan = build_resume_copy_plan(&db, Some(&recorder.session_id), false, None)?;
        assert_eq!(resume_plan.items.len(), 1);
        assert_eq!(resume_plan.items[0].rel_path, "a.bin");
        assert_eq!(resume_plan.left_label, "left");
        assert_eq!(resume_plan.right_label, "right");

        fs::remove_dir_all(root).ok();
        Ok(())
    }

    #[test]
    fn resume_plan_command_can_execute_without_out_json() -> Result<()> {
        let root = temp_dir("resume_execute");
        let db = root.join("resume.sqlite");
        let src = root.join("src");
        let dst = root.join("dst");
        fs::create_dir_all(&src)?;
        fs::create_dir_all(&dst)?;
        fs::write(src.join("a.bin"), b"abc")?;

        let plan = CopyPlan {
            mode: "copy-missing".to_string(),
            left_label: "left".to_string(),
            right_label: "right".to_string(),
            left_db: Some(db.display().to_string()),
            right_db: None,
            generated_at_ns: 0,
            summary: CopyPlanSummary {
                files_to_copy: 1,
                bytes_to_copy: 3,
                left_files: 1,
                right_files: 0,
            },
            items: vec![CopyPlanItem {
                rel_path: "a.bin".to_string(),
                file_type: "file".to_string(),
                size: 3,
                mtime_ns: 0,
                fast_hash: None,
            }],
        };
        let args = CopyRunArgs {
            source_root: src.clone(),
            destination_root: dst.clone(),
            backup_dir: None,
            overwrite: false,
            dry_run: false,
            stop_on_error: false,
            log: None,
            progress_every: 1000,
            size_only: false,
            hash: false,
            copy_links_as_files: false,
        };

        let recorder = ResumeRecorder::start(&plan, &args, 1)?.expect("recorder");
        let conn = open_db(&db)?;
        conn.execute(
            "INSERT OR REPLACE INTO files(label, rel_path, file_type, size, mtime_ns, fast_hash, scanned_at) VALUES('left','a.bin','file',3,0,NULL,0)",
            params![],
        )?;
        recorder.mark_status("a.bin", "failed", 0, Some("x"), true)?;

        resume_plan_command(ResumePlanArgs {
            db: db.clone(),
            list_sessions: false,
            stats: false,
            prune_completed: false,
            dry_run_prune: false,
            vacuum: false,
            session_id: Some(recorder.session_id),
            only_failed: false,
            max_attempts: None,
            jsonl_out: None,
            out_json: None,
            execute: true,
            from: Some(src),
            to: Some(dst),
            overwrite: false,
            dry_run: true,
            stop_on_error: false,
            policy: None,
            log: None,
            progress_every: 1000,
        })?;

        fs::remove_dir_all(root).ok();
        Ok(())
    }

    #[test]
    fn resume_jsonl_export_writes_filtered_rows() -> Result<()> {
        let root = temp_dir("resume_jsonl_export");
        let db = root.join("resume.sqlite");
        let src = root.join("src");
        let dst = root.join("dst");
        fs::create_dir_all(&src)?;
        fs::create_dir_all(&dst)?;
        let plan = CopyPlan {
            mode: "copy-missing".to_string(),
            left_label: "left".to_string(),
            right_label: "right".to_string(),
            left_db: Some(db.display().to_string()),
            right_db: None,
            generated_at_ns: 0,
            summary: CopyPlanSummary {
                files_to_copy: 2,
                bytes_to_copy: 2,
                left_files: 2,
                right_files: 0,
            },
            items: vec![],
        };
        let args = CopyRunArgs {
            source_root: src,
            destination_root: dst,
            backup_dir: None,
            overwrite: false,
            dry_run: false,
            stop_on_error: false,
            log: None,
            progress_every: 1000,
            size_only: false,
            hash: false,
            copy_links_as_files: false,
        };
        let recorder = ResumeRecorder::start(&plan, &args, 2)?.expect("recorder");
        recorder.mark_status("a.bin", "failed", 0, Some("x"), true)?;
        recorder.mark_status("b.bin", "done", 2, None, true)?;

        let out = root.join("resume.jsonl");
        resume_plan_command(ResumePlanArgs {
            db: db.clone(),
            list_sessions: false,
            stats: false,
            prune_completed: false,
            dry_run_prune: false,
            vacuum: false,
            session_id: Some(recorder.session_id),
            only_failed: true,
            max_attempts: None,
            jsonl_out: Some(out.clone()),
            out_json: None,
            execute: false,
            from: None,
            to: None,
            overwrite: false,
            dry_run: false,
            stop_on_error: false,
            policy: None,
            log: None,
            progress_every: 1000,
        })?;

        let raw = fs::read_to_string(out)?;
        let lines: Vec<&str> = raw.lines().collect();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("\"status\":\"failed\""));
        assert!(lines[0].contains("\"rel_path\":\"a.bin\""));

        fs::remove_dir_all(root).ok();
        Ok(())
    }

    #[test]
    fn resume_list_and_stats_output_paths_work() -> Result<()> {
        let root = temp_dir("resume_list_stats");
        let db = root.join("resume.sqlite");
        let src = root.join("src");
        let dst = root.join("dst");
        fs::create_dir_all(&src)?;
        fs::create_dir_all(&dst)?;
        fs::write(src.join("a.bin"), b"a")?;

        let plan = CopyPlan {
            mode: "copy-missing".to_string(),
            left_label: "left".to_string(),
            right_label: "right".to_string(),
            left_db: Some(db.display().to_string()),
            right_db: None,
            generated_at_ns: 0,
            summary: CopyPlanSummary {
                files_to_copy: 1,
                bytes_to_copy: 1,
                left_files: 1,
                right_files: 0,
            },
            items: vec![],
        };
        let args = CopyRunArgs {
            source_root: src,
            destination_root: dst,
            backup_dir: None,
            overwrite: false,
            dry_run: false,
            stop_on_error: false,
            log: None,
            progress_every: 1000,
            size_only: false,
            hash: false,
            copy_links_as_files: false,
        };
        let recorder = ResumeRecorder::start(&plan, &args, 1)?.expect("recorder");
        recorder.mark_status("a.bin", "failed", 0, Some("x"), true)?;

        let conn = open_db(&db)?;
        let sessions = list_resume_sessions(&conn)?;
        assert!(!sessions.is_empty());
        let stats = load_resume_session_stats(&conn, &recorder.session_id)?;
        assert_eq!(stats.failed, 1);

        fs::remove_dir_all(root).ok();
        Ok(())
    }

    #[test]
    fn resume_prune_completed_removes_done_and_skipped_rows() -> Result<()> {
        let root = temp_dir("resume_prune");
        let db = root.join("resume.sqlite");
        let src = root.join("src");
        let dst = root.join("dst");
        fs::create_dir_all(&src)?;
        fs::create_dir_all(&dst)?;

        let plan = CopyPlan {
            mode: "copy-missing".to_string(),
            left_label: "left".to_string(),
            right_label: "right".to_string(),
            left_db: Some(db.display().to_string()),
            right_db: None,
            generated_at_ns: 0,
            summary: CopyPlanSummary {
                files_to_copy: 3,
                bytes_to_copy: 3,
                left_files: 3,
                right_files: 0,
            },
            items: vec![],
        };
        let args = CopyRunArgs {
            source_root: src,
            destination_root: dst,
            backup_dir: None,
            overwrite: false,
            dry_run: false,
            stop_on_error: false,
            log: None,
            progress_every: 1000,
            size_only: false,
            hash: false,
            copy_links_as_files: false,
        };
        let recorder = ResumeRecorder::start(&plan, &args, 3)?.expect("recorder");
        recorder.mark_status("a.bin", "done", 1, None, true)?;
        recorder.mark_status("b.bin", "skipped_existing", 0, None, true)?;
        recorder.mark_status("c.bin", "failed", 0, Some("x"), true)?;

        let conn = open_db(&db)?;
        let result = prune_resume_completed_rows(&conn, Some(&recorder.session_id), false)?;
        assert_eq!(result.deleted_rows, 2);
        let stats = load_resume_session_stats(&conn, &recorder.session_id)?;
        assert_eq!(stats.failed, 1);
        assert_eq!(stats.done, 0);
        assert_eq!(stats.skipped_existing, 0);

        fs::remove_dir_all(root).ok();
        Ok(())
    }

    #[test]
    fn resume_prune_dry_run_reports_without_deleting() -> Result<()> {
        let root = temp_dir("resume_prune_dry");
        let db = root.join("resume.sqlite");
        let src = root.join("src");
        let dst = root.join("dst");
        fs::create_dir_all(&src)?;
        fs::create_dir_all(&dst)?;

        let plan = CopyPlan {
            mode: "copy-missing".to_string(),
            left_label: "left".to_string(),
            right_label: "right".to_string(),
            left_db: Some(db.display().to_string()),
            right_db: None,
            generated_at_ns: 0,
            summary: CopyPlanSummary {
                files_to_copy: 2,
                bytes_to_copy: 2,
                left_files: 2,
                right_files: 0,
            },
            items: vec![],
        };
        let args = CopyRunArgs {
            source_root: src,
            destination_root: dst,
            backup_dir: None,
            overwrite: false,
            dry_run: false,
            stop_on_error: false,
            log: None,
            progress_every: 1000,
            size_only: false,
            hash: false,
            copy_links_as_files: false,
        };
        let recorder = ResumeRecorder::start(&plan, &args, 2)?.expect("recorder");
        recorder.mark_status("a.bin", "done", 1, None, true)?;
        recorder.mark_status("b.bin", "failed", 0, Some("x"), true)?;

        let conn = open_db(&db)?;
        let dry = prune_resume_completed_rows(&conn, Some(&recorder.session_id), true)?;
        assert!(dry.dry_run);
        assert_eq!(dry.deleted_rows, 1);
        let stats = load_resume_session_stats(&conn, &recorder.session_id)?;
        assert_eq!(stats.done, 1);
        assert_eq!(stats.failed, 1);

        fs::remove_dir_all(root).ok();
        Ok(())
    }

    #[test]
    fn merge_plan_and_apply_dry_run_work() -> Result<()> {
        let root = temp_dir("merge_plan_apply");
        let imports = root.join("imports");
        let canonical = root.join("canonical");
        fs::create_dir_all(imports.join("A"))?;
        fs::create_dir_all(canonical.join("010001"))?;
        fs::write(imports.join("A/poc.bin"), b"abc")?;

        let actions = root.join("actions.csv");
        fs::write(
            &actions,
            concat!(
                "left_folder,rank,right_folder,confidence_tier,next_action,overlap_ratio,shared_hash_count,shared_normalized_file_name_count,shared_rel_file_count\n",
                "A,1,010001,similar,review and likely apply,0.77,3,2,2\n",
            ),
        )?;
        let plan_path = root.join("merge-plan.json");
        merge_plan_command(MergePlanArgs {
            actions_csv: actions,
            imports_root: imports.clone(),
            canonical_root: canonical.clone(),
            policy: MergePolicy::PreferNewer,
            out_json: plan_path.clone(),
        })?;
        assert!(plan_path.exists());
        merge_apply_command(MergeApplyArgs {
            plan: plan_path,
            dry_run: true,
        })?;

        fs::remove_dir_all(root).ok();
        Ok(())
    }

    #[test]
    fn merge_apply_materializes_directory_and_keep_both_variant() -> Result<()> {
        let root = temp_dir("merge_apply_materialize");
        let imports = root.join("imports");
        let canonical = root.join("canonical");
        fs::create_dir_all(imports.join("A/sub"))?;
        fs::create_dir_all(canonical.join("010001/sub"))?;
        fs::write(imports.join("A/sub/poc.bin"), b"abc")?;
        fs::write(canonical.join("010001/sub/poc.bin"), b"old")?;

        let actions = root.join("actions.csv");
        fs::write(
            &actions,
            concat!(
                "left_folder,rank,right_folder,confidence_tier,next_action,overlap_ratio,shared_hash_count,shared_normalized_file_name_count,shared_rel_file_count\n",
                "A,1,010001,similar,review and likely apply,0.77,3,2,2\n",
            ),
        )?;
        let apply_plan = root.join("apply-plan.json");
        merge_plan_command(MergePlanArgs {
            actions_csv: actions.clone(),
            imports_root: imports.clone(),
            canonical_root: canonical.clone(),
            policy: MergePolicy::PreferNewer,
            out_json: apply_plan.clone(),
        })?;
        merge_apply_command(MergeApplyArgs {
            plan: apply_plan,
            dry_run: false,
        })?;
        assert_eq!(fs::read(canonical.join("010001/sub/poc.bin"))?, b"abc");

        let keep_both_plan = root.join("keep-both-plan.json");
        merge_plan_command(MergePlanArgs {
            actions_csv: actions,
            imports_root: imports.clone(),
            canonical_root: canonical.clone(),
            policy: MergePolicy::KeepBoth,
            out_json: keep_both_plan.clone(),
        })?;
        merge_apply_command(MergeApplyArgs {
            plan: keep_both_plan,
            dry_run: false,
        })?;

        let keep_both_path = canonical.join("010001.from_import/sub/poc.bin");
        assert!(keep_both_path.exists());
        assert_eq!(fs::read(keep_both_path)?, b"abc");
        fs::remove_dir_all(root).ok();
        Ok(())
    }

    #[test]
    fn resume_plan_filters_failed_and_max_attempts() -> Result<()> {
        let root = temp_dir("resume_filters");
        let db = root.join("resume.sqlite");
        let src = root.join("src");
        let dst = root.join("dst");
        fs::create_dir_all(&src)?;
        fs::create_dir_all(&dst)?;
        fs::write(src.join("a.bin"), b"a")?;
        fs::write(src.join("b.bin"), b"b")?;
        fs::write(src.join("c.bin"), b"c")?;

        let plan = CopyPlan {
            mode: "copy-missing".to_string(),
            left_label: "left".to_string(),
            right_label: "right".to_string(),
            left_db: Some(db.display().to_string()),
            right_db: None,
            generated_at_ns: 0,
            summary: CopyPlanSummary {
                files_to_copy: 3,
                bytes_to_copy: 3,
                left_files: 3,
                right_files: 0,
            },
            items: vec![],
        };
        let args = CopyRunArgs {
            source_root: src.clone(),
            destination_root: dst.clone(),
            backup_dir: None,
            overwrite: false,
            dry_run: false,
            stop_on_error: false,
            log: None,
            progress_every: 1000,
            size_only: false,
            hash: false,
            copy_links_as_files: false,
        };
        let recorder = ResumeRecorder::start(&plan, &args, 3)?.expect("recorder");
        let conn = open_db(&db)?;
        conn.execute(
            "INSERT OR REPLACE INTO files(label, rel_path, file_type, size, mtime_ns, fast_hash, scanned_at) VALUES('left','a.bin','file',1,0,NULL,0)",
            params![],
        )?;
        conn.execute(
            "INSERT OR REPLACE INTO files(label, rel_path, file_type, size, mtime_ns, fast_hash, scanned_at) VALUES('left','b.bin','file',1,0,NULL,0)",
            params![],
        )?;
        conn.execute(
            "INSERT OR REPLACE INTO files(label, rel_path, file_type, size, mtime_ns, fast_hash, scanned_at) VALUES('left','c.bin','file',1,0,NULL,0)",
            params![],
        )?;
        recorder.mark_status("a.bin", "failed", 0, Some("x"), true)?;
        recorder.mark_status("b.bin", "failed", 0, Some("x"), true)?;
        recorder.mark_status("b.bin", "failed", 0, Some("x"), true)?;
        recorder.mark_status("c.bin", "pending", 0, None, true)?;

        let failed_only = build_resume_copy_plan(&db, Some(&recorder.session_id), true, None)?;
        assert_eq!(failed_only.items.len(), 2);
        let failed_limited =
            build_resume_copy_plan(&db, Some(&recorder.session_id), true, Some(1))?;
        assert_eq!(failed_limited.items.len(), 1);
        assert_eq!(failed_limited.items[0].rel_path, "a.bin");

        fs::remove_dir_all(root).ok();
        Ok(())
    }

    #[test]
    fn infer_archive_family_detects_compound_nested_formats() {
        assert_eq!(
            infer_archive_family("backup.tar.gz"),
            Some("tar.gz".to_string())
        );
        assert_eq!(
            infer_archive_family("snapshot.tar.xz"),
            Some("tar.xz".to_string())
        );
        assert_eq!(
            infer_archive_family("bundle.tar.bz2"),
            Some("tar.bz2".to_string())
        );
        assert_eq!(
            infer_archive_family("image.archive.img.raw"),
            Some("img.raw".to_string())
        );
        assert_eq!(
            infer_archive_family("archive.zip+txt"),
            Some("zip+txt".to_string())
        );
        assert_eq!(infer_archive_family("notes.txt"), None);
        assert_eq!(
            dossier_archive_signature("bundle.tar.gz"),
            Some("tar".to_string())
        );
        assert_eq!(
            dossier_archive_signature("image.zip+txt"),
            Some("zip".to_string())
        );
    }

    #[test]
    fn virtual_archive_paths_capture_nested_family_shape() {
        assert_eq!(
            build_virtual_archive_path("fw/qcom_payload_final.tar.gz"),
            "qcom_payload_final/@tar/gz"
        );
        assert_eq!(
            build_virtual_archive_path("snapshots/image.archive.img.raw"),
            "image.archive/@img/img"
        );
        assert_eq!(archive_family_depth("tar.gz"), 2);
        assert_eq!(archive_family_depth("zip+txt"), 2);
    }

    #[test]
    fn virtual_archive_member_identity_normalizes_member_like_names() {
        assert_eq!(
            normalize_virtual_archive_member_identity(r"./FW Pack/../Payload Final (V2)"),
            "payload_final_v2"
        );
        assert_eq!(
            normalize_virtual_archive_member_identity(r"nested\\INNER.dir\\part-01"),
            "nested/inner_dir/part_01"
        );
        assert_eq!(
            normalize_virtual_archive_member_identity("///...///"),
            "member"
        );
    }

    #[test]
    fn virtual_archive_member_paths_capture_nested_archive_naming_edges() {
        assert_eq!(
            build_virtual_archive_member_path(r"FW\QCOM Payload Final.TAR.GZ"),
            "qcom_payload_final/@tar/gz"
        );
        assert_eq!(
            build_virtual_archive_member_path("snapshots/image.archive.img.raw"),
            "image_archive/@img/img"
        );
        assert_eq!(
            build_virtual_archive_member_path("./nested/../mix+chars v1.2.zip+txt"),
            "mix_chars_v1_2/@zip/txt"
        );
    }

    #[test]
    fn scan_populates_virtual_archive_member_manifest_and_query() -> Result<()> {
        let root = temp_dir("virtual_archive_manifest_scan");
        let source_root = root.join("source");
        fs::create_dir_all(source_root.join("fw"))?;
        fs::create_dir_all(source_root.join("docs"))?;
        fs::write(source_root.join("fw/payload.tar.gz"), b"abc")?;
        fs::write(source_root.join("docs/readme.txt"), b"plain")?;

        let db = root.join("scan.sqlite");
        scan_command(ScanArgs {
            db: db.clone(),
            label: "left".to_string(),
            root: source_root,
            exclude_prefixes: Vec::new(),
            exclude_if_present: Vec::new(),
            policy: None,
            hash: false,
        })?;

        let conn = open_readonly_db(&db)?;
        let archives = load_virtual_archive_entries(&conn, "left")?;
        assert_eq!(archives.len(), 1);
        assert_eq!(archives[0].path, "fw/payload.tar.gz");
        assert_eq!(archives[0].virtual_path, "payload/@tar/gz");
        assert_eq!(archives[0].virtual_member, "payload/@tar/gz");
        assert_eq!(archives[0].archive_family, Some("tar.gz".to_string()));
        assert_eq!(archives[0].archive_depth, 2);

        fs::remove_dir_all(root).ok();
        Ok(())
    }

    #[test]
    fn scan_updates_virtual_archive_manifest_when_archive_disappears() -> Result<()> {
        let root = temp_dir("virtual_archive_manifest_update");
        let source_root = root.join("source");
        fs::create_dir_all(source_root.join("fw"))?;
        fs::write(source_root.join("fw/payload.tar.gz"), b"abc")?;

        let db = root.join("scan.sqlite");
        scan_command(ScanArgs {
            db: db.clone(),
            label: "left".to_string(),
            root: source_root.clone(),
            exclude_prefixes: Vec::new(),
            exclude_if_present: Vec::new(),
            policy: None,
            hash: false,
        })?;

        fs::remove_file(source_root.join("fw/payload.tar.gz"))?;
        fs::write(source_root.join("fw/payload.txt"), b"abc")?;
        scan_command(ScanArgs {
            db: db.clone(),
            label: "left".to_string(),
            root: source_root,
            exclude_prefixes: Vec::new(),
            exclude_if_present: Vec::new(),
            policy: None,
            hash: false,
        })?;

        let conn = open_readonly_db(&db)?;
        let archives = load_virtual_archive_entries(&conn, "left")?;
        assert!(archives.is_empty());

        fs::remove_dir_all(root).ok();
        Ok(())
    }

    #[test]
    fn dossier_matching_uses_archive_signature_for_compressed_payload_variants() {
        let left_rows = vec![FileRecord {
            rel_path: "left/source-a.tar.gz".to_string(),
            file_type: "file".to_string(),
            size: 101,
            mtime_ns: 1,
            fast_hash: None,
        }];
        let right_rows = vec![FileRecord {
            rel_path: "right/delta_payload.tar.xz".to_string(),
            file_type: "file".to_string(),
            size: 102,
            mtime_ns: 2,
            fast_hash: None,
        }];

        let left_signatures = build_folder_signatures(&left_rows);
        let right_signatures = build_folder_signatures(&right_rows);
        let matches = build_dossier_matches(&left_signatures, &right_signatures, 1);

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].left_folder, "left");
        assert_eq!(matches[0].right_folder, "right");
        assert_eq!(matches[0].shared_binaryity_count, 1);
        assert_eq!(matches[0].shared_archive_family_count, 0);
        assert!(matches[0].overlap_weight > 0.0);
    }

    #[test]
    fn dossier_matching_uses_fingerprint_profiles_for_renamed_paths() {
        let left_rows = vec![
            FileRecord {
                rel_path: "case-final-v2/readme-final.txt".to_string(),
                file_type: "file".to_string(),
                size: 101,
                mtime_ns: 1,
                fast_hash: Some("hash-readme".to_string()),
            },
            FileRecord {
                rel_path: "case-final-v2/chain.bin".to_string(),
                file_type: "file".to_string(),
                size: 202,
                mtime_ns: 2,
                fast_hash: Some("hash-chain".to_string()),
            },
        ];
        let right_rows = vec![
            FileRecord {
                rel_path: "case_v2_case/readme.txt".to_string(),
                file_type: "file".to_string(),
                size: 111,
                mtime_ns: 10,
                fast_hash: Some("hash-readme".to_string()),
            },
            FileRecord {
                rel_path: "case_v2_case/chain-final.bin".to_string(),
                file_type: "file".to_string(),
                size: 222,
                mtime_ns: 11,
                fast_hash: Some("hash-chain".to_string()),
            },
        ];

        let mut left_profiles = HashMap::new();
        left_profiles.insert(
            "case-final-v2/readme-final.txt".to_string(),
            build_file_fingerprint_profile(
                &left_rows[0].rel_path,
                &left_rows[0].file_type,
                left_rows[0].size,
                left_rows[0].fast_hash.as_deref(),
            ),
        );
        left_profiles.insert(
            "case-final-v2/chain.bin".to_string(),
            build_file_fingerprint_profile(
                &left_rows[1].rel_path,
                &left_rows[1].file_type,
                left_rows[1].size,
                left_rows[1].fast_hash.as_deref(),
            ),
        );
        let mut right_profiles = HashMap::new();
        right_profiles.insert(
            "case_v2_case/readme.txt".to_string(),
            build_file_fingerprint_profile(
                &right_rows[0].rel_path,
                &right_rows[0].file_type,
                right_rows[0].size,
                right_rows[0].fast_hash.as_deref(),
            ),
        );
        right_profiles.insert(
            "case_v2_case/chain-final.bin".to_string(),
            build_file_fingerprint_profile(
                &right_rows[1].rel_path,
                &right_rows[1].file_type,
                right_rows[1].size,
                right_rows[1].fast_hash.as_deref(),
            ),
        );

        let left_signatures = build_folder_signatures_with_profiles(&left_rows, &left_profiles);
        let right_signatures = build_folder_signatures_with_profiles(&right_rows, &right_profiles);
        let matches = build_dossier_matches(&left_signatures, &right_signatures, 1);

        assert_eq!(left_signatures.len(), 1);
        assert_eq!(right_signatures.len(), 1);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].left_folder, "case-final-v2");
        assert_eq!(matches[0].right_folder, "case_v2_case");
        assert!(matches[0].overlap_weight > 0.0);
    }

    #[test]
    fn dossier_matching_falls_back_to_derived_profiles() {
        let left_rows = vec![
            FileRecord {
                rel_path: "case-final-v2/readme-final.txt".to_string(),
                file_type: "file".to_string(),
                size: 101,
                mtime_ns: 1,
                fast_hash: Some("hash-readme".to_string()),
            },
            FileRecord {
                rel_path: "case-final-v2/chain.bin".to_string(),
                file_type: "file".to_string(),
                size: 202,
                mtime_ns: 2,
                fast_hash: Some("hash-chain".to_string()),
            },
        ];
        let right_rows = vec![
            FileRecord {
                rel_path: "case_v2_case/readme.txt".to_string(),
                file_type: "file".to_string(),
                size: 111,
                mtime_ns: 10,
                fast_hash: Some("hash-readme".to_string()),
            },
            FileRecord {
                rel_path: "case_v2_case/chain-final.bin".to_string(),
                file_type: "file".to_string(),
                size: 222,
                mtime_ns: 11,
                fast_hash: Some("hash-chain".to_string()),
            },
        ];

        let left_signatures = build_folder_signatures_with_profiles(&left_rows, &HashMap::new());
        let right_signatures = build_folder_signatures_with_profiles(&right_rows, &HashMap::new());
        let matches = build_dossier_matches(&left_signatures, &right_signatures, 1);

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].left_folder, "case-final-v2");
        assert_eq!(matches[0].right_folder, "case_v2_case");
        assert!(matches[0].overlap_weight > 0.0);
    }

    #[test]
    fn dossier_matching_uses_persisted_profiles_from_databases() -> Result<()> {
        let root = temp_dir("dossier_db_profiles");
        let left_root = root.join("source");
        let right_root = root.join("destination");
        fs::create_dir_all(left_root.join("Case-Final-20240101"))?;
        fs::create_dir_all(right_root.join("case-v2").join("copy"))?;

        fs::write(
            left_root.join("Case-Final-20240101/readme-0134.log"),
            b"left-a",
        )?;
        fs::write(
            left_root.join("Case-Final-20240101/settings-0189.cfg"),
            b"left-b",
        )?;
        fs::write(right_root.join("case-v2/copy/readme-zzz.txt"), b"right-a")?;
        fs::write(right_root.join("case-v2/copy/settings-x.bin"), b"right-b")?;

        let left_db = root.join("left.sqlite");
        let right_db = root.join("right.sqlite");

        scan_command(ScanArgs {
            db: left_db.clone(),
            label: "left".to_string(),
            root: left_root,
            exclude_prefixes: Vec::new(),
            exclude_if_present: Vec::new(),
            policy: None,
            hash: false,
        })?;
        scan_command(ScanArgs {
            db: right_db.clone(),
            label: "right".to_string(),
            root: right_root,
            exclude_prefixes: Vec::new(),
            exclude_if_present: Vec::new(),
            policy: None,
            hash: false,
        })?;

        let left_conn = open_db(&left_db)?;
        let right_conn = open_db(&right_db)?;

        left_conn.execute(
            "UPDATE file_fingerprints SET normalized_name = 'readme_case', normalized_folder = 'case', is_binary = 0, is_archive = 0 WHERE label = 'left' AND rel_path = 'Case-Final-20240101/readme-0134.log'",
            params![],
        )?;
        left_conn.execute(
            "UPDATE file_fingerprints SET normalized_name = 'settings_case', normalized_folder = 'case', is_binary = 0, is_archive = 0 WHERE label = 'left' AND rel_path = 'Case-Final-20240101/settings-0189.cfg'",
            params![],
        )?;
        right_conn.execute(
            "UPDATE file_fingerprints SET normalized_name = 'readme_case', normalized_folder = 'case', is_binary = 0, is_archive = 0 WHERE label = 'right' AND rel_path = 'case-v2/copy/readme-zzz.txt'",
            params![],
        )?;
        right_conn.execute(
            "UPDATE file_fingerprints SET normalized_name = 'settings_case', normalized_folder = 'case', is_binary = 0, is_archive = 0 WHERE label = 'right' AND rel_path = 'case-v2/copy/settings-x.bin'",
            params![],
        )?;

        let left_rows = load_label(&open_readonly_db(&left_db)?, "left")?;
        let right_rows = load_label(&open_readonly_db(&right_db)?, "right")?;
        let left_profiles = load_file_fingerprint_profiles(&open_readonly_db(&left_db)?, "left")?;
        let right_profiles =
            load_file_fingerprint_profiles(&open_readonly_db(&right_db)?, "right")?;

        let fallback_left_signatures =
            build_folder_signatures_with_profiles(&left_rows, &HashMap::new());
        let fallback_right_signatures =
            build_folder_signatures_with_profiles(&right_rows, &HashMap::new());
        let fallback_matches =
            build_dossier_matches(&fallback_left_signatures, &fallback_right_signatures, 1);
        assert_eq!(fallback_matches.len(), 1);
        assert_eq!(fallback_matches[0].shared_normalized_file_name_count, 0);
        assert_eq!(fallback_matches[0].shared_normalized_parent_folder_count, 0);

        let left_signatures = build_folder_signatures_with_profiles(&left_rows, &left_profiles);
        let right_signatures = build_folder_signatures_with_profiles(&right_rows, &right_profiles);
        let matches = build_dossier_matches(&left_signatures, &right_signatures, 1);

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].left_folder, "Case-Final-20240101");
        assert_eq!(matches[0].right_folder, "case-v2/copy");
        assert!(matches[0].overlap_weight > 0.0);
        assert!(matches[0].shared_normalized_file_name_count > 0);
        assert!(
            fallback_matches[0].shared_normalized_file_name_count
                < matches[0].shared_normalized_file_name_count
        );

        fs::remove_dir_all(root).ok();
        Ok(())
    }

    #[test]
    fn dossier_matching_falls_back_when_some_profiles_are_missing() -> Result<()> {
        let root = temp_dir("dossier_profiles_partial_missing");
        let left_root = root.join("source");
        let right_root = root.join("destination");
        fs::create_dir_all(left_root.join("Case-Final-20240101"))?;
        fs::create_dir_all(right_root.join("case-v2").join("copy"))?;

        fs::write(
            left_root.join("Case-Final-20240101/readme-0134.log"),
            b"left-a",
        )?;
        fs::write(
            left_root.join("Case-Final-20240101/settings-0189.cfg"),
            b"left-b",
        )?;
        fs::write(right_root.join("case-v2/copy/readme-zzz.txt"), b"right-a")?;
        fs::write(right_root.join("case-v2/copy/settings-x.bin"), b"right-b")?;

        let left_db = root.join("left.sqlite");
        let right_db = root.join("right.sqlite");

        scan_command(ScanArgs {
            db: left_db.clone(),
            label: "left".to_string(),
            root: left_root,
            exclude_prefixes: Vec::new(),
            exclude_if_present: Vec::new(),
            policy: None,
            hash: false,
        })?;
        scan_command(ScanArgs {
            db: right_db.clone(),
            label: "right".to_string(),
            root: right_root,
            exclude_prefixes: Vec::new(),
            exclude_if_present: Vec::new(),
            policy: None,
            hash: false,
        })?;

        let left_conn = open_db(&left_db)?;
        let right_conn = open_db(&right_db)?;

        left_conn.execute(
            "UPDATE file_fingerprints SET normalized_name = 'readme_case', normalized_folder = 'case', is_binary = 0, is_archive = 0 WHERE label = 'left' AND rel_path = 'Case-Final-20240101/readme-0134.log'",
            params![],
        )?;
        left_conn.execute(
            "UPDATE file_fingerprints SET normalized_name = 'settings_case', normalized_folder = 'case', is_binary = 0, is_archive = 0 WHERE label = 'left' AND rel_path = 'Case-Final-20240101/settings-0189.cfg'",
            params![],
        )?;
        right_conn.execute(
            "UPDATE file_fingerprints SET normalized_name = 'readme_case', normalized_folder = 'case', is_binary = 0, is_archive = 0 WHERE label = 'right' AND rel_path = 'case-v2/copy/readme-zzz.txt'",
            params![],
        )?;
        right_conn.execute(
            "UPDATE file_fingerprints SET normalized_name = 'settings_case', normalized_folder = 'case', is_binary = 0, is_archive = 0 WHERE label = 'right' AND rel_path = 'case-v2/copy/settings-x.bin'",
            params![],
        )?;

        let left_rows = load_label(&open_readonly_db(&left_db)?, "left")?;
        let right_rows = load_label(&open_readonly_db(&right_db)?, "right")?;

        let full_left_profiles =
            load_file_fingerprint_profiles(&open_readonly_db(&left_db)?, "left")?;
        let full_right_profiles =
            load_file_fingerprint_profiles(&open_readonly_db(&right_db)?, "right")?;

        let mut partial_left_profiles = full_left_profiles.clone();
        partial_left_profiles.remove("Case-Final-20240101/settings-0189.cfg");

        let full_left = build_folder_signatures_with_profiles(&left_rows, &full_left_profiles);
        let full_right = build_folder_signatures_with_profiles(&right_rows, &full_right_profiles);
        let full_matches = build_dossier_matches(&full_left, &full_right, 1);

        let partial_left =
            build_folder_signatures_with_profiles(&left_rows, &partial_left_profiles);
        let partial_right = full_right;
        let partial_matches = build_dossier_matches(&partial_left, &partial_right, 1);

        assert_eq!(full_matches.len(), 1);
        assert_eq!(partial_matches.len(), 1);
        assert!(partial_matches[0].overlap_weight > 0.0);
        assert!(
            full_matches[0].shared_normalized_file_name_count
                > partial_matches[0].shared_normalized_file_name_count
        );
        assert!(partial_matches[0].overlap_weight < full_matches[0].overlap_weight);

        fs::remove_dir_all(root).ok();
        Ok(())
    }

    #[test]
    fn load_file_fingerprint_profiles_returns_normalized_values() -> Result<()> {
        let root = temp_dir("load_fingerprint_profiles");
        let source_root = root.join("source");
        fs::create_dir_all(source_root.join("case-final-20240101"))?;
        fs::write(
            source_root.join("case-final-20240101/readme_final.txt"),
            b"one",
        )?;
        fs::write(
            source_root.join("case-final-20240101/notes_old.bin"),
            b"two",
        )?;
        fs::write(
            source_root.join("case-final-20240101/bundle_20240101.cfg"),
            b"three",
        )?;

        let db = root.join("scan.sqlite");
        scan_command(ScanArgs {
            db: db.clone(),
            label: "left".to_string(),
            root: source_root,
            exclude_prefixes: Vec::new(),
            exclude_if_present: Vec::new(),
            policy: None,
            hash: false,
        })?;

        let conn = open_readonly_db(&db)?;
        let profiles = load_file_fingerprint_profiles(&conn, "left")?;

        let readme_profile = profiles.get("case-final-20240101/readme_final.txt");
        assert!(readme_profile.is_some());
        let readme_profile = readme_profile.expect("readme profile");
        assert_eq!(readme_profile.normalized_name, "readme");
        assert_eq!(readme_profile.normalized_folder, "case");
        assert_eq!(readme_profile.ext, "txt".to_string());
        assert_eq!(readme_profile.language, "markdown");
        assert_eq!(readme_profile.size_class, "small");

        let notes_profile = profiles.get("case-final-20240101/notes_old.bin");
        assert!(notes_profile.is_some());
        let notes_profile = notes_profile.expect("notes profile");
        assert_eq!(notes_profile.normalized_name, "notes");
        assert_eq!(notes_profile.normalized_folder, "case");
        assert_eq!(notes_profile.ext, "bin".to_string());
        assert_eq!(notes_profile.language, "unknown");
        assert_eq!(notes_profile.size_class, "small");

        let bundle_profile = profiles.get("case-final-20240101/bundle_20240101.cfg");
        assert!(bundle_profile.is_some());
        let bundle_profile = bundle_profile.expect("bundle profile");
        assert_eq!(bundle_profile.normalized_name, "bundle");
        assert_eq!(bundle_profile.normalized_folder, "case");
        assert_eq!(bundle_profile.ext, "cfg".to_string());
        assert_eq!(bundle_profile.language, "unknown");
        assert_eq!(bundle_profile.size_class, "small");

        fs::remove_dir_all(root).ok();
        Ok(())
    }

    #[test]
    fn profile_cache_usage_counts_hits_and_misses() {
        let rows = vec![
            FileRecord {
                rel_path: "alpha/readme.txt".to_string(),
                file_type: "file".to_string(),
                size: 10,
                mtime_ns: 1,
                fast_hash: None,
            },
            FileRecord {
                rel_path: "alpha/missing.bin".to_string(),
                file_type: "file".to_string(),
                size: 11,
                mtime_ns: 2,
                fast_hash: None,
            },
        ];
        let mut profiles = HashMap::new();
        profiles.insert(
            "alpha/readme.txt".to_string(),
            build_file_fingerprint_profile("alpha/readme.txt", "file", 10, None),
        );

        let counters = profile_cache_usage(&rows, &profiles);
        assert_eq!(counters.hits, 1);
        assert_eq!(counters.misses, 1);
        assert_eq!(counters.analytics.coverage.total_rows, 2);
        assert_eq!(counters.analytics.coverage.profile_rows, 1);
        assert!((counters.analytics.coverage.coverage_ratio - 0.5).abs() < 1e-9);
        assert_eq!(
            counters.analytics.descriptor_density.profiled_rows,
            counters.hits
        );
    }

    #[test]
    fn profile_cache_usage_tracks_descriptor_density() {
        let rows = vec![
            FileRecord {
                rel_path: "alpha/readme.txt".to_string(),
                file_type: "file".to_string(),
                size: 10,
                mtime_ns: 1,
                fast_hash: None,
            },
            FileRecord {
                rel_path: "alpha/archive.tar.gz".to_string(),
                file_type: "file".to_string(),
                size: 11,
                mtime_ns: 2,
                fast_hash: None,
            },
            FileRecord {
                rel_path: "alpha/missing.bin".to_string(),
                file_type: "file".to_string(),
                size: 12,
                mtime_ns: 3,
                fast_hash: None,
            },
        ];
        let mut profiles = HashMap::new();
        profiles.insert(
            "alpha/readme.txt".to_string(),
            FileFingerprintProfile {
                normalized_name: "readme".to_string(),
                normalized_folder: "alpha".to_string(),
                ext: "txt".to_string(),
                is_binary: false,
                is_archive: false,
                archive_family: None,
                language: "markdown".to_string(),
                size_class: "small".to_string(),
                binary_signature: None,
                binary_descriptor: None,
                text_signature: Some("markdown:readme".to_string()),
                archive_signature: None,
            },
        );
        profiles.insert(
            "alpha/archive.tar.gz".to_string(),
            FileFingerprintProfile {
                normalized_name: "archive".to_string(),
                normalized_folder: "alpha".to_string(),
                ext: "gz".to_string(),
                is_binary: true,
                is_archive: true,
                archive_family: Some("tar".to_string()),
                language: "unknown".to_string(),
                size_class: "small".to_string(),
                binary_signature: Some("sig".to_string()),
                binary_descriptor: Some("desc".to_string()),
                text_signature: None,
                archive_signature: Some("tar+gz".to_string()),
            },
        );

        let counters = profile_cache_usage(&rows, &profiles);
        assert_eq!(counters.hits, 2);
        assert_eq!(counters.misses, 1);
        assert!((counters.analytics.coverage.coverage_ratio - (2.0 / 3.0)).abs() < 1e-9);
        assert_eq!(
            counters.analytics.descriptor_density.with_binary_descriptor,
            1
        );
        assert_eq!(counters.analytics.descriptor_density.with_text_signature, 1);
        assert_eq!(
            counters.analytics.descriptor_density.with_archive_signature,
            1
        );
        assert_eq!(counters.analytics.descriptor_density.with_any_descriptor, 2);
        assert!((counters.analytics.descriptor_density.any_descriptor_ratio - 1.0).abs() < 1e-9);
    }

    #[test]
    fn compare_summary_serialization_includes_schema_and_cache_metrics() {
        let summary = CompareSummary {
            report_schema: COMPARE_SUMMARY_REPORT_SCHEMA.to_string(),
            report_version: REPORT_VERSION_V1,
            left_label: "left".to_string(),
            right_label: "right".to_string(),
            left_files: 10,
            right_files: 12,
            same_path_same_meta: 6,
            same_path_changed: 2,
            left_only: 2,
            right_only: 4,
            cache_metrics: ReportCacheMetrics {
                left_profile_cache: CacheUsageCounters {
                    hits: 7,
                    misses: 3,
                    ..Default::default()
                },
                right_profile_cache: CacheUsageCounters {
                    hits: 9,
                    misses: 3,
                    ..Default::default()
                },
            },
        };

        let value = serde_json::to_value(&summary).expect("serialize compare summary");
        assert_eq!(
            value.get("report_schema").and_then(|v| v.as_str()),
            Some(COMPARE_SUMMARY_REPORT_SCHEMA)
        );
        assert_eq!(
            value.get("report_version").and_then(|v| v.as_u64()),
            Some(REPORT_VERSION_V1 as u64)
        );
        assert_eq!(
            value
                .pointer("/cache_metrics/left_profile_cache/hits")
                .and_then(|v| v.as_u64()),
            Some(7)
        );
        assert_eq!(
            value
                .pointer("/cache_metrics/right_profile_cache/misses")
                .and_then(|v| v.as_u64()),
            Some(3)
        );
        assert_eq!(
            value
                .pointer("/cache_metrics/left_profile_cache/analytics/coverage/coverage_ratio")
                .and_then(|v| v.as_f64()),
            Some(0.0)
        );
        assert_eq!(
            value
                .pointer(
                    "/cache_metrics/right_profile_cache/analytics/descriptor_density/with_any_descriptor"
                )
                .and_then(|v| v.as_u64()),
            Some(0)
        );
    }

    #[test]
    fn dossier_report_serialization_includes_schema_and_cache_metrics() {
        let report = DossierReport {
            report_schema: DOSSIER_REPORT_SCHEMA.to_string(),
            report_version: REPORT_VERSION_V1,
            left_db: "left.sqlite".to_string(),
            right_db: "right.sqlite".to_string(),
            left_label: "left".to_string(),
            right_label: "right".to_string(),
            top_k: 15,
            min_confidence: DossierConfidenceTier::Manual,
            only_action: None,
            left_folder_count: 2,
            right_folder_count: 2,
            archive_signal_candidates: 1,
            archive_signal_ratio: 0.5,
            cache_metrics: ReportCacheMetrics {
                left_profile_cache: CacheUsageCounters {
                    hits: 5,
                    misses: 1,
                    ..Default::default()
                },
                right_profile_cache: CacheUsageCounters {
                    hits: 4,
                    misses: 2,
                    ..Default::default()
                },
            },
            left_profile_cache: CacheUsageCounters {
                hits: 5,
                misses: 1,
                ..Default::default()
            },
            right_profile_cache: CacheUsageCounters {
                hits: 4,
                misses: 2,
                ..Default::default()
            },
            confidence_counts: DossierConfidenceCounts::default(),
            candidates: Vec::new(),
        };

        let value = serde_json::to_value(&report).expect("serialize dossier report");
        assert_eq!(
            value.get("report_schema").and_then(|v| v.as_str()),
            Some(DOSSIER_REPORT_SCHEMA)
        );
        assert_eq!(
            value.get("report_version").and_then(|v| v.as_u64()),
            Some(REPORT_VERSION_V1 as u64)
        );
        assert_eq!(
            value
                .pointer("/cache_metrics/left_profile_cache/hits")
                .and_then(|v| v.as_u64()),
            Some(5)
        );
        assert_eq!(
            value
                .pointer("/cache_metrics/right_profile_cache/misses")
                .and_then(|v| v.as_u64()),
            Some(2)
        );
        assert_eq!(
            value
                .pointer("/left_profile_cache/analytics/coverage/total_rows")
                .and_then(|v| v.as_u64()),
            Some(0)
        );
        assert_eq!(
            value
                .pointer("/right_profile_cache/analytics/descriptor_density/any_descriptor_ratio")
                .and_then(|v| v.as_f64()),
            Some(0.0)
        );
    }

    #[test]
    fn dossier_matching_respects_policy_filters_and_renamed_folders() {
        let policy = ExcludePolicy {
            directory_prefixes: vec!["noise".to_string()],
            folder_name_additions: Vec::new(),
            subtree_overrides: HashMap::new(),
            enabled: true,
        };

        let left_rows = vec![
            FileRecord {
                rel_path: "renamed-alpha/readme.txt".to_string(),
                file_type: "file".to_string(),
                size: 101,
                mtime_ns: 1,
                fast_hash: Some("hash-readme".to_string()),
            },
            FileRecord {
                rel_path: "renamed-alpha/app.bin".to_string(),
                file_type: "file".to_string(),
                size: 202,
                mtime_ns: 2,
                fast_hash: Some("hash-app".to_string()),
            },
            FileRecord {
                rel_path: "noise/ignored.bin".to_string(),
                file_type: "file".to_string(),
                size: 303,
                mtime_ns: 3,
                fast_hash: Some("hash-noise".to_string()),
            },
        ];
        let right_rows = vec![
            FileRecord {
                rel_path: "renamed-beta/readme.txt".to_string(),
                file_type: "file".to_string(),
                size: 111,
                mtime_ns: 10,
                fast_hash: Some("hash-readme".to_string()),
            },
            FileRecord {
                rel_path: "renamed-beta/app.bin".to_string(),
                file_type: "file".to_string(),
                size: 222,
                mtime_ns: 11,
                fast_hash: Some("hash-app".to_string()),
            },
            FileRecord {
                rel_path: "noise/ignored.bin".to_string(),
                file_type: "file".to_string(),
                size: 333,
                mtime_ns: 12,
                fast_hash: Some("hash-noise".to_string()),
            },
        ];

        let left_rows: Vec<FileRecord> = left_rows
            .into_iter()
            .filter(|row| !should_exclude_path(&row.rel_path, &policy))
            .collect();
        let right_rows: Vec<FileRecord> = right_rows
            .into_iter()
            .filter(|row| !should_exclude_path(&row.rel_path, &policy))
            .collect();

        assert_eq!(left_rows.len(), 2);
        assert_eq!(right_rows.len(), 2);
        assert!(
            left_rows
                .iter()
                .all(|row| !row.rel_path.starts_with("noise/"))
        );
        assert!(
            right_rows
                .iter()
                .all(|row| !row.rel_path.starts_with("noise/"))
        );

        let left_signatures = build_folder_signatures(&left_rows);
        let right_signatures = build_folder_signatures(&right_rows);
        let matches = build_dossier_matches(&left_signatures, &right_signatures, 3);

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].left_folder, "renamed-alpha");
        assert_eq!(matches[0].right_folder, "renamed-beta");
        assert_eq!(matches[0].shared_rel_file_count, 2);
    }

    #[test]
    fn dossier_alias_command_still_parses() {
        let cli = Cli::parse_from([
            "nightindex",
            "intel",
            "--left-db",
            "/tmp/left.sqlite",
            "--right-db",
            "/tmp/right.sqlite",
            "--left",
            "left",
            "--right",
            "right",
        ]);
        match cli.command {
            Commands::Dossier(args) => {
                assert_eq!(args.left_db, PathBuf::from("/tmp/left.sqlite"));
                assert_eq!(args.right_db, PathBuf::from("/tmp/right.sqlite"));
            }
            _ => panic!("intel alias did not resolve to dossier"),
        }
    }

    #[test]
    fn binary_alias_and_compat_aliases_parse() {
        let cli = Cli::parse_from([
            "ndex",
            "sync",
            "--left-db",
            "/tmp/left.sqlite",
            "--right-db",
            "/tmp/right.sqlite",
            "--left",
            "left",
            "--right",
            "right",
            "--from",
            "/tmp/source",
            "--to",
            "/tmp/dest",
        ]);
        match cli.command {
            Commands::SyncCopyMissing(args) => {
                assert_eq!(args.left_db, PathBuf::from("/tmp/left.sqlite"));
                assert_eq!(args.right_db, PathBuf::from("/tmp/right.sqlite"));
                assert_eq!(args.from, PathBuf::from("/tmp/source"));
                assert_eq!(args.to, PathBuf::from("/tmp/dest"));
            }
            _ => panic!("ndex sync alias did not resolve to sync-copy-missing"),
        }

        let cli = Cli::parse_from(["nightindex", "rsync", "--dry-run", "src", "dst"]);
        match cli.command {
            Commands::Rsync(args) => {
                let runtime = parse_compat_copy_flags(&args, "rsync").expect("compat parse");
                assert!(runtime.dry_run);
                assert_eq!(runtime.source, PathBuf::from("src"));
                assert_eq!(runtime.destination, PathBuf::from("dst"));
            }
            _ => panic!("rsync compatibility command did not parse"),
        }
    }

    #[test]
    fn open_readonly_db_does_not_create_db() {
        let root = temp_dir("readonly");
        let db_path = root.join("missing.sqlite");
        assert!(!db_path.exists());
        assert!(open_readonly_db(&db_path).is_err());
        assert!(!db_path.exists());
        assert!(open_db(&db_path).is_ok());
        assert!(db_path.exists());
        assert!(open_readonly_db(&db_path).is_ok());
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn signature_cache_reuses_and_invalidates_fingerprint_profiles() -> Result<()> {
        let root = temp_dir("signature_cache");
        let db_path = root.join("cache.sqlite");
        let conn = open_db(&db_path)?;
        let profile =
            build_file_fingerprint_profile("010001/poc_final.py", "file", 128, Some("hash-a"));

        store_cached_file_fingerprint_profile(
            &conn,
            "010001/poc_final.py",
            "file",
            128,
            10,
            Some("hash-a"),
            &profile,
            99,
        )?;

        let cached = load_cached_file_fingerprint_profile(
            &conn,
            "010001/poc_final.py",
            "file",
            128,
            10,
            Some("hash-a"),
        )?
        .expect("cache hit");
        assert_eq!(cached.language, "python");
        assert_eq!(cached.normalized_name, profile.normalized_name);
        assert_eq!(cached.text_signature, Some("python:poc".to_string()));

        let changed_mtime = load_cached_file_fingerprint_profile(
            &conn,
            "010001/poc_final.py",
            "file",
            128,
            11,
            Some("hash-a"),
        )?;
        assert!(changed_mtime.is_none());

        let rows: i64 =
            conn.query_row("SELECT COUNT(*) FROM signature_cache", [], |row| row.get(0))?;
        assert_eq!(rows, 1);
        fs::remove_dir_all(root).ok();
        Ok(())
    }

    #[test]
    fn fingerprint_profiles_include_cached_signature_classes() {
        let archive = build_file_fingerprint_profile(
            "fw/qcom_payload_final.tar.gz",
            "file",
            4096,
            Some("hash-archive"),
        );
        assert!(archive.is_archive);
        assert_eq!(archive.archive_family, Some("tar.gz".to_string()));
        assert_eq!(archive.binary_signature, Some("tar:small:fw".to_string()));
        assert_eq!(
            archive.binary_descriptor,
            Some("tar:small:fw:hash-archi".to_string())
        );
        assert_eq!(
            archive.archive_signature,
            Some("tar:qcom_payload".to_string())
        );

        let source = build_file_fingerprint_profile(
            "01_EXPLOITS/010001/poc_final.py",
            "file",
            512,
            Some("hash-source"),
        );
        assert_eq!(source.language, "python");
        assert_eq!(source.text_signature, Some("python:poc".to_string()));
        assert!(source.binary_signature.is_none());
        assert!(source.binary_descriptor.is_none());
    }

    #[test]
    fn binary_descriptor_is_stable_and_hash_sensitive() {
        let a = infer_binary_descriptor(
            "firmware/bin/agent.sys",
            Some("sys"),
            None,
            "1m",
            Some("abcdef1234567890"),
            None,
        );
        let b = infer_binary_descriptor(
            "firmware/bin/agent.sys",
            Some("sys"),
            None,
            "1m",
            Some("abcdef1234567890"),
            None,
        );
        let c = infer_binary_descriptor(
            "firmware/bin/agent.sys",
            Some("sys"),
            None,
            "1m",
            Some("123456abcdef7890"),
            None,
        );
        assert_eq!(a, "sys:1m:bin:abcdef1234");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn binary_sample_signature_is_deterministic() -> Result<()> {
        let root = temp_dir("binary_sample_deterministic");
        fs::create_dir_all(&root)?;
        let path = root.join("blob.bin");
        let payload = (0..8192).map(|i| (i % 251) as u8).collect::<Vec<_>>();
        fs::write(&path, payload)?;
        let size = fs::metadata(&path)?.len();
        let first = infer_binary_sample_signature_from_file(&path, size);
        let second = infer_binary_sample_signature_from_file(&path, size);
        assert!(first.is_some());
        assert_eq!(first, second);
        fs::remove_dir_all(root).ok();
        Ok(())
    }

    #[test]
    fn binary_sample_signature_respects_budget_limit() -> Result<()> {
        let root = temp_dir("binary_sample_budget");
        fs::create_dir_all(&root)?;
        let path = root.join("huge.bin");
        let oversized = BINARY_DESCRIPTOR_MAX_SAMPLE_FILE_BYTES + 1;
        let file = File::create(&path)?;
        file.set_len(oversized)?;
        assert!(infer_binary_sample_signature_from_file(&path, oversized).is_none());
        fs::remove_dir_all(root).ok();
        Ok(())
    }

    #[test]
    fn binary_sample_signature_handles_tiny_binary() -> Result<()> {
        let root = temp_dir("binary_sample_tiny");
        fs::create_dir_all(&root)?;
        let path = root.join("tiny.bin");
        fs::write(&path, [0xAB])?;
        let size = fs::metadata(&path)?.len();
        let first = infer_binary_sample_signature_from_file(&path, size);
        let second = infer_binary_sample_signature_from_file(&path, size);
        assert!(first.is_some());
        assert_eq!(first, second);
        fs::remove_dir_all(root).ok();
        Ok(())
    }

    #[test]
    fn dossier_signatures_emit_binary_descriptor_token() {
        let rows = vec![FileRecord {
            rel_path: "firmware/bin/agent.sys".to_string(),
            file_type: "file".to_string(),
            size: 512 * 1024,
            mtime_ns: 1,
            fast_hash: Some("abcdef1234567890".to_string()),
        }];
        let signatures = build_folder_signatures(&rows);
        let folder = signatures.get("firmware/bin").expect("folder signature");
        assert!(folder.tokens.contains_key("BINDESC:sys:1m:bin:abcdef1234"));
    }

    #[test]
    fn binary_descriptor_is_bounded_for_long_noisy_paths() {
        let long_segment = "Firmware-VERY-LONG-SEGMENT_with_noise_v1234_final_copy_20250101";
        let rel_path = format!("{long_segment}/{long_segment}/{long_segment}.sys");
        let descriptor = infer_binary_descriptor(
            &rel_path,
            Some("sys"),
            None,
            "1m",
            Some("abcdef1234567890"),
            Some("fedcba0987654321fedcba0987654321"),
        );
        assert!(descriptor.len() <= DESCRIPTOR_MAX_COMPOSITE_LEN);
        assert!(descriptor.starts_with("sys:1m:"));
    }

    #[test]
    fn semantic_text_signature_extracts_import_and_function_tokens() {
        let content = r#"
import os
from pathlib import Path

def collect_results(path):
    return Path(path)
"#;
        let signature = infer_semantic_text_signature_from_content("python", "collector", content);
        assert!(signature.starts_with("python:collector"));
        assert!(signature.contains("i:os+pathlib") || signature.contains("i:pathlib+os"));
        assert!(signature.contains("f:collect_results"));
    }

    #[test]
    fn semantic_text_signature_suppresses_low_signal_and_malformed_lines() {
        let content = r#"
import a
import tmp
import urllib.request
def x(v):
key = "value"
aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa = 1
[x]
[main-service]
bad::::line
"#;
        let signature = infer_semantic_text_signature_from_content("python", "collector", content);
        assert!(signature.contains("python:collector"));
        assert!(!signature.contains("i:a"));
        assert!(!signature.contains("i:tmp"));
        assert!(!signature.contains("f:x"));
        assert!(!signature.contains("k:key"));
        assert!(signature.contains("i:urllib"));
        assert!(signature.contains("s:main_service"));
        assert!(signature.len() <= DESCRIPTOR_MAX_COMPOSITE_LEN);
    }

    #[test]
    fn archive_payload_signature_is_bounded_and_normalized() {
        let rel_path = "fw/Very Long Payload Name FINAL Copy 20250101.tar.gz";
        let signature =
            infer_archive_payload_signature(rel_path, "tar.gz").expect("archive payload signature");
        assert!(signature.starts_with("tar:"));
        assert!(signature.len() <= DESCRIPTOR_MAX_COMPOSITE_LEN);
    }

    #[test]
    fn inspect_cache_command_emits_label_metrics() -> Result<()> {
        let root = temp_dir("inspect_cache_cmd");
        let source = root.join("src");
        fs::create_dir_all(&source)?;
        fs::write(
            source.join("a.py"),
            "import os\n\ndef run(path):\n    return os.path.abspath(path)\n",
        )?;
        fs::write(source.join("b.bin"), vec![0u8; 1024])?;
        let db = root.join("manifest.sqlite");

        scan_command(ScanArgs {
            db: db.clone(),
            label: "left".to_string(),
            root: source,
            exclude_prefixes: vec![],
            exclude_if_present: vec![],
            policy: None,
            hash: true,
        })?;

        let out_json = root.join("inspect.json");
        inspect_cache_command(InspectCacheArgs {
            db: db.clone(),
            label: Some("left".to_string()),
            out_json: Some(out_json.clone()),
        })?;

        let raw = fs::read_to_string(&out_json)?;
        assert!(raw.contains("\"report_schema\": \"nightindex.inspect_cache\""));
        assert!(raw.contains("\"label\": \"left\""));
        assert!(raw.contains("\"with_text_signature\""));
        assert!(raw.contains("\"with_binary_signature\""));
        fs::remove_dir_all(root).ok();
        Ok(())
    }

    #[test]
    fn archive_member_diff_reports_exact_and_left_only() -> Result<()> {
        let root = temp_dir("archive_member_diff");
        let left_root = root.join("left");
        let right_root = root.join("right");
        fs::create_dir_all(&left_root)?;
        fs::create_dir_all(&right_root)?;
        fs::write(left_root.join("same.tar.gz"), b"same")?;
        fs::write(left_root.join("only_left.tar.gz"), b"left")?;
        fs::write(right_root.join("same.tar.gz"), b"same")?;

        let left_db = root.join("left.sqlite");
        let right_db = root.join("right.sqlite");
        scan_command(ScanArgs {
            db: left_db.clone(),
            label: "left".to_string(),
            root: left_root,
            exclude_prefixes: vec![],
            exclude_if_present: vec![],
            policy: None,
            hash: true,
        })?;
        scan_command(ScanArgs {
            db: right_db.clone(),
            label: "right".to_string(),
            root: right_root,
            exclude_prefixes: vec![],
            exclude_if_present: vec![],
            policy: None,
            hash: true,
        })?;

        let out_json = root.join("archive_diff.json");
        archive_member_diff_command(ArchiveMemberDiffArgs {
            left_db: left_db.clone(),
            right_db: right_db.clone(),
            left: "left".to_string(),
            right: "right".to_string(),
            out_json: Some(out_json.clone()),
            out_csv: None,
        })?;

        let raw = fs::read_to_string(out_json)?;
        assert!(raw.contains("\"report_schema\": \"nightindex.archive_member_diff\""));
        assert!(raw.contains("\"exact_member_matches\": 1"));
        assert!(raw.contains("\"left_only_count\": 1"));
        fs::remove_dir_all(root).ok();
        Ok(())
    }
}
