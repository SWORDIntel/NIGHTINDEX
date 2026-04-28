use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_yaml;
use walkdir::WalkDir;

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

const ARCHIVE_EXTENSIONS: &[&str] = &[
    ".tar.gz", ".tar.xz", ".zip", ".7z", ".tar", ".rar", ".img", ".iso", ".bin", ".raw", ".dmg",
    ".apk", ".jar", ".ovpn", ".cpio",
];

const DOSSIER_NAME_TOKEN_WEIGHT: f64 = 1.0;
const DOSSIER_STEM_TOKEN_WEIGHT: f64 = 0.35;
const DOSSIER_EXTENSION_TOKEN_WEIGHT: f64 = 0.2;
const DOSSIER_EXTENSION_STEM_TOKEN_WEIGHT: f64 = 0.55;
const DOSSIER_HASH_TOKEN_WEIGHT: f64 = 2.5;
const DOSSIER_FOLDER_TOKEN_WEIGHT: f64 = 0.1;
const DOSSIER_FOLDER_PREFIX_TOKEN_WEIGHT: f64 = 0.05;
const DOSSIER_FOLDER_DEPTH_TOKEN_WEIGHT: f64 = 0.02;

#[derive(Parser)]
#[command(name = "nightindex")]
#[command(about = "Indexed recovery copy for hostile file trees", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Scan(ScanArgs),
    CompareSummary(CompareSummaryArgs),
    Brief(BriefArgs),
    #[command(alias = "intel")]
    Dossier(DossierArgs),
    #[command(alias = "extcheck")]
    ExtractCheck(ExtractCheckArgs),
    #[command(alias = "plan")]
    PlanCopyMissing(PlanCopyMissingArgs),
    ExecuteCopyMissing(ExecuteCopyMissingArgs),
    #[command(alias = "execute")]
    ExecutePlan(ExecutePlanArgs),
    #[command(name = "sync-copy-missing", alias = "sync")]
    SyncCopyMissing(SyncCopyMissingArgs),
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
    #[arg(long)]
    out_json: Option<PathBuf>,
    #[arg(long)]
    out_csv: Option<PathBuf>,
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

#[derive(Debug, Clone)]
struct FileRecord {
    rel_path: String,
    size: u64,
    mtime_ns: i64,
    fast_hash: Option<String>,
}

#[derive(Debug, Serialize)]
struct CompareSummary {
    left_label: String,
    right_label: String,
    left_files: usize,
    right_files: usize,
    same_path_same_meta: usize,
    same_path_changed: usize,
    left_only: usize,
    right_only: usize,
}

#[derive(Debug, Serialize)]
struct BriefSummary {
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
    left_size: u64,
    right_size: u64,
    left_mtime_ns: i64,
    right_mtime_ns: i64,
    left_fast_hash: Option<String>,
    right_fast_hash: Option<String>,
}

#[derive(Debug, Serialize)]
struct ExtractCheckReport {
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

#[derive(Debug, Serialize, Deserialize)]
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

#[derive(Debug, Serialize, Deserialize)]
struct CopyPlanSummary {
    files_to_copy: usize,
    bytes_to_copy: u64,
    left_files: usize,
    right_files: usize,
}

#[derive(Debug, Serialize, Deserialize)]
struct CopyPlanItem {
    rel_path: String,
    size: u64,
    mtime_ns: i64,
    fast_hash: Option<String>,
}

struct CopyRunArgs {
    source_root: PathBuf,
    destination_root: PathBuf,
    overwrite: bool,
    dry_run: bool,
    stop_on_error: bool,
    log: Option<PathBuf>,
    progress_every: usize,
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
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum CopyEventAction {
    SourceMissing,
    SkipExisting,
    SkipConflict,
    Copy,
    Overwrite,
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
}

#[derive(Debug, Serialize)]
struct DossierReport {
    left_db: String,
    right_db: String,
    left_label: String,
    right_label: String,
    top_k: usize,
    left_folder_count: usize,
    right_folder_count: usize,
    candidates: Vec<DossierMatch>,
}

#[derive(Default)]
struct DossierMatchState {
    shared_weight: f64,
    shared_file_name_weight: f64,
    shared_file_stem_weight: f64,
    shared_file_ext_weight: f64,
    shared_ext_stem_weight: f64,
    shared_hash_weight: f64,
    shared_folder_weight: f64,
    shared_rel_file_count: usize,
}

#[derive(Copy, Clone)]
enum DossierTokenFamily {
    ExactFileName,
    FileStem,
    FileExtension,
    ExtensionStem,
    Hash,
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
        Commands::PlanCopyMissing(args) => plan_copy_missing_command(args),
        Commands::ExecuteCopyMissing(args) => execute_copy_missing_command(args),
        Commands::ExecutePlan(args) => execute_plan_command(args),
        Commands::SyncCopyMissing(args) => sync_copy_missing_command(args),
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
    let scanned_at = now_ns()?;
    let conn = open_db(&args.db)?;

    println!(
        "[scan] label={} root={} hash={}",
        args.label,
        root.display(),
        args.hash
    );
    if !exclude_prefixes.is_empty() {
        println!("[scan] excludes={}", exclude_prefixes.join(", "));
    }

    let mut files_seen = 0usize;
    let mut hashed = 0usize;
    let mut reused = 0usize;
    let mut excluded = 0usize;
    let mut errors = 0usize;

    for entry in WalkDir::new(&root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| should_walk(entry.path(), &root, &policy))
    {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                eprintln!("[scan] walk failed: {err}");
                errors += 1;
                continue;
            }
        };

        if entry.file_type().is_dir() {
            continue;
        }
        if !entry.file_type().is_file() {
            continue;
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

        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(err) => {
                eprintln!("[scan] stat failed: {}: {err}", entry.path().display());
                errors += 1;
                continue;
            }
        };

        let size = metadata.len();
        let mtime_ns = metadata
            .modified()
            .ok()
            .and_then(system_time_to_ns)
            .unwrap_or_default();

        let existing = conn
            .query_row(
                "SELECT size, mtime_ns, fast_hash FROM files WHERE label = ?1 AND rel_path = ?2",
                params![&args.label, &rel_path],
                |row| {
                    Ok((
                        row.get::<_, u64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()?;

        let fast_hash = if let Some((old_size, old_mtime_ns, old_hash)) = existing {
            if old_size == size && old_mtime_ns == mtime_ns && (!args.hash || old_hash.is_some()) {
                reused += 1;
                old_hash
            } else if args.hash {
                hashed += 1;
                Some(blake3_file(entry.path())?)
            } else {
                None
            }
        } else if args.hash {
            hashed += 1;
            Some(blake3_file(entry.path())?)
        } else {
            None
        };

        conn.execute(
            r#"
            INSERT INTO files(label, rel_path, file_type, size, mtime_ns, fast_hash, scanned_at)
            VALUES(?1, ?2, 'file', ?3, ?4, ?5, ?6)
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
                size,
                mtime_ns,
                fast_hash,
                scanned_at
            ],
        )?;

        files_seen += 1;
        if files_seen % 500 == 0 {
            println!(
                "[scan] files={} hashed={} reused={} excluded={} errors={}",
                files_seen, hashed, reused, excluded, errors
            );
        }
    }

    println!(
        "[scan] done files={} hashed={} reused={} excluded={} errors={}",
        files_seen, hashed, reused, excluded, errors
    );
    Ok(())
}

fn compare_summary_command(args: CompareSummaryArgs) -> Result<()> {
    let left_conn = open_db(&args.left_db)?;
    let right_conn = open_db(&args.right_db)?;

    let left_rows = load_label(&left_conn, &args.left)?;
    let right_rows = load_label(&right_conn, &args.right)?;

    let (summary, _, _) =
        build_compare_and_copy_summary(&left_rows, &right_rows, &args.left, &args.right)?;

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

    let (summary, files_to_copy, bytes_to_copy) =
        build_compare_and_copy_summary(&left_rows, &right_rows, &args.left, &args.right)?;

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
    Ok(())
}

fn extract_check_command(args: ExtractCheckArgs) -> Result<()> {
    let left_conn = open_db(&args.left_db)?;
    let right_conn = open_db(&args.right_db)?;

    let left_rows = load_label(&left_conn, &args.left)?;
    let right_rows = load_label(&right_conn, &args.right)?;

    let left_archives = build_archive_entries(&left_rows)?;
    let right_archives = build_archive_entries(&right_rows)?;

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

        entries.push(ExtractCheckEntry {
            path: row.rel_path.clone(),
            folder,
            stem,
            size: row.size,
            mtime_ns: row.mtime_ns,
            fast_hash: row.fast_hash.clone(),
        });
    }
    Ok(entries)
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
    }
    csv
}

fn dossier_command(args: DossierArgs) -> Result<()> {
    let left_conn = open_readonly_db(&args.left_db)?;
    let right_conn = open_readonly_db(&args.right_db)?;
    let policy = load_exclude_policy(args.policy.as_deref())?;
    let left_rows = load_label(&left_conn, &args.left)?;
    let right_rows = load_label(&right_conn, &args.right)?;

    let left_rows: Vec<FileRecord> = left_rows
        .into_iter()
        .filter(|row| !should_exclude_path(&row.rel_path, &policy))
        .collect();
    let right_rows: Vec<FileRecord> = right_rows
        .into_iter()
        .filter(|row| !should_exclude_path(&row.rel_path, &policy))
        .collect();

    let left_signatures = build_folder_signatures(&left_rows);
    let right_signatures = build_folder_signatures(&right_rows);

    let candidates = build_dossier_matches(&left_signatures, &right_signatures, args.top_k);

    let report = DossierReport {
        left_db: args.left_db.display().to_string(),
        right_db: args.right_db.display().to_string(),
        left_label: args.left,
        right_label: args.right,
        top_k: args.top_k,
        left_folder_count: left_signatures.len(),
        right_folder_count: right_signatures.len(),
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
        left_label: left.to_string(),
        right_label: right.to_string(),
        left_files: left_map.len(),
        right_files: right_map.len(),
        same_path_same_meta,
        same_path_changed,
        left_only,
        right_only,
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
                    }
                    DossierTokenFamily::FileStem => {
                        state.shared_file_stem_weight += shared;
                    }
                    DossierTokenFamily::FileExtension => {
                        state.shared_file_ext_weight += shared;
                    }
                    DossierTokenFamily::ExtensionStem => {
                        state.shared_ext_stem_weight += shared;
                    }
                    DossierTokenFamily::Hash => {
                        state.shared_hash_weight += shared;
                    }
                    DossierTokenFamily::Folder => {
                        state.shared_folder_weight += shared;
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
            ranked.push((
                DossierMatch {
                    left_folder: left_folder.clone(),
                    right_folder,
                    overlap_weight: state.shared_weight,
                    left_weight: left_signature.total_weight,
                    right_weight: right_signature.total_weight,
                    overlap_ratio,
                    shared_rel_file_count: state.shared_rel_file_count,
                },
                state,
            ));
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
                        .shared_hash_weight
                        .partial_cmp(&a_state.shared_hash_weight)
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
        "left_folder,right_folder,overlap_weight,left_weight,right_weight,overlap_ratio,shared_rel_file_count\n",
    );

    for item in matches {
        let _ = std::fmt::Write::write_fmt(
            &mut csv,
            format_args!(
                "{},{},{:.4},{:.4},{:.4},{:.6},{}\n",
                csv_escape(&item.left_folder),
                csv_escape(&item.right_folder),
                item.overlap_weight,
                item.left_weight,
                item.right_weight,
                item.overlap_ratio,
                item.shared_rel_file_count
            ),
        );
    }
    csv
}

fn build_folder_signatures(rows: &[FileRecord]) -> HashMap<String, FolderSignature> {
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
    let Some((prefix, _)) = token.split_once(':') else {
        return DossierTokenFamily::Other;
    };
    match prefix {
        "N" => DossierTokenFamily::ExactFileName,
        "S" => DossierTokenFamily::FileStem,
        "E" => DossierTokenFamily::FileExtension,
        "ES" => DossierTokenFamily::ExtensionStem,
        "H" => DossierTokenFamily::Hash,
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
            overwrite: args.overwrite,
            dry_run: args.dry_run,
            stop_on_error: args.stop_on_error,
            log: args.log,
            progress_every: args.progress_every,
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
                    overwrite: args.overwrite,
                    dry_run: args.dry_run,
                    stop_on_error: args.stop_on_error,
                    log: args.log,
                    progress_every: args.progress_every,
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
            overwrite: args.overwrite,
            dry_run: args.dry_run,
            stop_on_error: args.stop_on_error,
            log: args.log,
            progress_every: args.progress_every,
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
    let write_event = |log: &mut Option<std::fs::File>, event: &CopyEvent| -> Result<()> {
        if let Some(writer) = log {
            let payload = serde_json::to_vec(event).context("serialize copy event")?;
            writer.write_all(&payload)?;
            writer.write_all(b"\n")?;
        }
        Ok(())
    };

    for (index, item) in items_to_copy.iter().enumerate() {
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
        match fs::metadata(&destination_path) {
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

        let is_overwrite = args.overwrite;
        let mut action = CopyEventAction::Copy;

        if destination_exists {
            if let Some(metadata) = destination_metadata.as_ref() {
                if metadata.is_file() {
                    let mut same_file = false;
                    if metadata.len() == item.size {
                        let destination_mtime = metadata
                            .modified()
                            .ok()
                            .and_then(system_time_to_ns)
                            .filter(|mtime| *mtime == item.mtime_ns);

                        if destination_mtime.is_some() {
                            same_file = true;
                        } else if let Some(expected_hash) = item.fast_hash.as_deref() {
                            same_file = blake3_file(&destination_path)? == expected_hash;
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
                                bytes: metadata.len(),
                                dry_run: args.dry_run,
                                overwrite: args.overwrite,
                                reason: Some(format!(
                                    "destination conflict: existing size {}",
                                    metadata.len()
                                )),
                            },
                        )?;
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
            copied_bytes += item.size;

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
                    bytes: item.size,
                    dry_run: true,
                    overwrite: args.overwrite,
                    reason: None,
                },
            )?;
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

        match fs::copy(&source_path, &destination_path) {
            Ok(bytes_written) => {
                copied += 1;
                copied_bytes += bytes_written;
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
                        bytes: bytes_written,
                        dry_run: false,
                        overwrite: args.overwrite,
                        reason: None,
                    },
                )?;
            }
            Err(err) => {
                failed += 1;
                if args.stop_on_error {
                    return Err(err)
                        .with_context(|| format!("failed copying {}", source_path.display()));
                }
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
                        reason: Some(err.to_string()),
                    },
                )?;
                eprintln!(
                    "[err] copy failed: {} -> {}: {}",
                    source_path.display(),
                    destination_path.display(),
                    err
                );
            }
        }
    }

    Ok(CopyExecutionSummary {
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
    })
}

fn open_db(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let conn =
        Connection::open(path).with_context(|| format!("failed to open db {}", path.display()))?;
    conn.execute_batch(SCHEMA)?;
    Ok(conn)
}

fn open_readonly_db(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("failed to open db {}", path.display()))?;
    Ok(conn)
}

fn load_label(conn: &Connection, label: &str) -> Result<Vec<FileRecord>> {
    let mut stmt = conn.prepare(
        "SELECT rel_path, size, mtime_ns, fast_hash FROM files WHERE label = ?1 ORDER BY rel_path",
    )?;
    let rows = stmt.query_map(params![label], |row| {
        Ok(FileRecord {
            rel_path: row.get(0)?,
            size: row.get(1)?,
            mtime_ns: row.get(2)?,
            fast_hash: row.get(3)?,
        })
    })?;

    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
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

fn should_walk(path: &Path, root: &Path, policy: &ExcludePolicy) -> bool {
    if path == root {
        return true;
    }
    let rel = match path.strip_prefix(root) {
        Ok(path) => path_to_slash(path),
        Err(_) => return true,
    };
    !should_exclude_path(&rel, policy)
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
