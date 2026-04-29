use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use serde_json::json;

use super::CopyExecutionSummary;

const COPY_SCHEMA_VERSION: u32 = 3;

fn human_bytes(value: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = value as f64;
    let mut idx = 0usize;
    while value >= 1024.0 && idx < UNITS.len() - 1 {
        value /= 1024.0;
        idx += 1;
    }
    format!("{value:.2} {}", UNITS[idx])
}

fn human_bytes_f64(value: f64) -> String {
    if !value.is_finite() || value <= 0.0 {
        return "0 B".to_string();
    }
    human_bytes(value.round() as u64)
}

fn human_duration_ns(value: i64) -> String {
    if value <= 0 {
        return "00m 00s".to_string();
    }
    let seconds = value / 1_000_000_000;
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let secs = seconds % 60;
    if days > 0 {
        return format!("{days}d {hours:02}h {minutes:02}m");
    }
    if hours > 0 {
        return format!("{hours:02}h {minutes:02}m {secs:02}s");
    }
    format!("{minutes:02}m {secs:02}s")
}

fn rate_from_elapsed(bytes: u64, elapsed_ns: i64) -> Option<f64> {
    if bytes == 0 || elapsed_ns <= 0 {
        return None;
    }
    Some((bytes as f64) / (elapsed_ns as f64 / 1_000_000_000.0))
}

pub fn format_start_line(
    mode: &str,
    planned_files: usize,
    planned_bytes: u64,
    dry_run: bool,
    overwrite: bool,
) -> String {
    format!(
        "[copy start] mode={mode} files={} bytes={} dry_run={} overwrite={}",
        planned_files,
        human_bytes(planned_bytes),
        dry_run,
        overwrite,
    )
}

pub struct CopyProgressSnapshot {
    pub planned_files: usize,
    pub planned_bytes: u64,
    pub completed_files: usize,
    pub copied_files: usize,
    pub skipped_existing: usize,
    pub skipped_conflict: usize,
    pub overwritten_files: usize,
    pub missing_source: usize,
    pub failed_files: usize,
    pub copied_bytes: u64,
    pub elapsed_ns: i64,
}

impl CopyProgressSnapshot {
    fn rate_bps(&self) -> Option<f64> {
        rate_from_elapsed(self.copied_bytes, self.elapsed_ns)
    }

    fn eta_ns(&self) -> Option<i64> {
        let rate = self.rate_bps()?;
        if rate <= 0.0 {
            return None;
        }
        let remaining = self.planned_bytes.saturating_sub(self.copied_bytes);
        Some(((remaining as f64) / rate * 1_000_000_000.0).round() as i64)
    }
}

pub fn format_progress_line(snapshot: &CopyProgressSnapshot) -> String {
    let rate = snapshot
        .rate_bps()
        .map(human_bytes_f64)
        .unwrap_or_else(|| "n/a".to_string());
    let eta = snapshot
        .eta_ns()
        .map(human_duration_ns)
        .unwrap_or_else(|| "n/a".to_string());
    format!(
        "[copy progress] {}/{} files | copied={} | rate={}/s | eta={} | skipped={} conflict={} missing={} failed={}",
        snapshot.completed_files,
        snapshot.planned_files,
        human_bytes(snapshot.copied_bytes),
        rate,
        eta,
        snapshot.skipped_existing,
        snapshot.skipped_conflict,
        snapshot.missing_source,
        snapshot.failed_files,
    )
}

pub fn format_summary_line(summary: &CopyExecutionSummary, elapsed_ns: i64) -> String {
    let rate = rate_from_elapsed(summary.copied_bytes, elapsed_ns)
        .map(human_bytes_f64)
        .unwrap_or_else(|| "n/a".to_string());
    format!(
        "[copy summary] mode={} dry_run={} overwrite={} files={}/{} copied={} skipped={} conflict={} overwritten={} missing={} failed={} elapsed={} rate={}/s",
        summary.mode,
        summary.dry_run,
        summary.overwrite,
        summary.copied_files,
        summary.planned_files,
        human_bytes(summary.copied_bytes),
        summary.skipped_existing,
        summary.skipped_conflict,
        summary.overwritten_files,
        summary.missing_source,
        summary.failed_files,
        human_duration_ns(elapsed_ns),
        rate,
    )
}

pub fn write_copy_progress_event(
    log: &mut Option<File>,
    snapshot: &CopyProgressSnapshot,
) -> Result<()> {
    write_json_event(
        log,
        &json!({
            "event": "copy_progress",
            "schema_version": COPY_SCHEMA_VERSION,
            "planned_files": snapshot.planned_files,
            "planned_bytes": snapshot.planned_bytes,
            "completed_files": snapshot.completed_files,
            "copied_files": snapshot.copied_files,
            "skipped_existing": snapshot.skipped_existing,
            "skipped_conflict": snapshot.skipped_conflict,
            "overwritten_files": snapshot.overwritten_files,
            "missing_source": snapshot.missing_source,
            "failed_files": snapshot.failed_files,
            "copied_bytes": snapshot.copied_bytes,
            "elapsed_ns": snapshot.elapsed_ns,
            "bytes_per_second": snapshot.rate_bps(),
            "eta_ns": snapshot.eta_ns(),
        }),
    )
}

pub fn write_copy_summary_event(
    log: &mut Option<File>,
    summary: &CopyExecutionSummary,
    elapsed_ns: i64,
) -> Result<()> {
    write_json_event(
        log,
        &json!({
            "event": "copy_summary",
            "schema_version": COPY_SCHEMA_VERSION,
            "mode": summary.mode,
            "dry_run": summary.dry_run,
            "overwrite": summary.overwrite,
            "planned_files": summary.planned_files,
            "copied_files": summary.copied_files,
            "skipped_existing": summary.skipped_existing,
            "skipped_conflict": summary.skipped_conflict,
            "overwritten_files": summary.overwritten_files,
            "missing_source": summary.missing_source,
            "failed_files": summary.failed_files,
            "copied_bytes": summary.copied_bytes,
            "deleted_files": summary.deleted_files,
            "deleted_bytes": summary.deleted_bytes,
            "elapsed_ns": elapsed_ns,
            "bytes_per_second": rate_from_elapsed(summary.copied_bytes, elapsed_ns),
        }),
    )
}

pub fn write_json_event(log: &mut Option<File>, event: &serde_json::Value) -> Result<()> {
    if let Some(writer) = log.as_mut() {
        serde_json::to_writer(&mut *writer, event).context("serialize copy log event")?;
        writer.write_all(b"\n")?;
    }
    Ok(())
}

#[derive(Debug)]
pub struct CopyStager {
    pid: u32,
    run_nonce: u128,
    next_seq: u64,
}

#[derive(Debug)]
pub struct CopyStage {
    temp_dir: PathBuf,
    temp_path: PathBuf,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CopyFinalizeOutcome {
    Committed,
    SkippedConflict { reason: String },
}

impl CopyStager {
    pub fn new() -> Self {
        let run_nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or_default();
        Self {
            pid: std::process::id(),
            run_nonce,
            next_seq: 0,
        }
    }

    pub fn stage(&mut self, destination: &Path) -> Result<CopyStage> {
        let parent = destination.parent().ok_or_else(|| {
            anyhow!(
                "destination has no parent directory: {}",
                destination.display()
            )
        })?;
        let final_name = destination
            .file_name()
            .unwrap_or_else(|| OsStr::new("copy"));

        for _attempt in 0..1024u32 {
            let seq = self.next_seq;
            self.next_seq += 1;
            let temp_dir = parent.join(format!(
                ".nightindex-copy-p{}-{:x}-{}",
                self.pid, self.run_nonce, seq
            ));
            match fs::create_dir(&temp_dir) {
                Ok(()) => {
                    let temp_path = temp_dir.join(final_name);
                    return Ok(CopyStage {
                        temp_dir,
                        temp_path,
                    });
                }
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(err) => {
                    return Err(err).with_context(|| {
                        format!("failed to create temp stage {}", temp_dir.display())
                    });
                }
            }
        }

        anyhow::bail!(
            "failed to allocate unique copy staging directory for {}",
            destination.display()
        );
    }
}

impl CopyStage {
    #[cfg(test)]
    pub fn temp_dir(&self) -> &Path {
        &self.temp_dir
    }

    pub fn temp_path(&self) -> &Path {
        &self.temp_path
    }

    pub fn finalize(self, destination: &Path, overwrite: bool) -> Result<CopyFinalizeOutcome> {
        if !overwrite {
            match fs::symlink_metadata(destination) {
                Ok(_) => {
                    return Ok(CopyFinalizeOutcome::SkippedConflict {
                        reason: format!(
                            "destination appeared before finalization: {}",
                            destination.display()
                        ),
                    });
                }
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => {
                    return Err(err)
                        .with_context(|| format!("failed to recheck {}", destination.display()));
                }
            }
        }

        fs::rename(&self.temp_path, destination).with_context(|| {
            format!(
                "failed to finalize staged copy {} -> {}",
                self.temp_path.display(),
                destination.display()
            )
        })?;
        Ok(CopyFinalizeOutcome::Committed)
    }
}

impl Drop for CopyStage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.temp_dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn temp_dir(prefix: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "nightindex-copy-exec-test-{}-{}-{}",
            prefix,
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir_all(&root).expect("create temp dir");
        root
    }

    #[test]
    fn stage_uses_pid_unique_directory_and_cleans_up() -> Result<()> {
        let root = temp_dir("stage");
        let destination = root.join("nested/out.txt");
        fs::create_dir_all(destination.parent().expect("parent"))?;

        let mut stager = CopyStager::new();
        let stage = stager.stage(&destination)?;
        assert!(
            stage
                .temp_dir()
                .starts_with(&destination.parent().expect("parent"))
        );
        assert!(stage.temp_path().starts_with(stage.temp_dir()));

        let temp_dir = stage.temp_dir().to_path_buf();
        drop(stage);
        assert!(!temp_dir.exists());
        fs::remove_dir_all(&root).ok();
        Ok(())
    }

    #[test]
    fn finalize_skips_race_conflict_without_touching_destination() -> Result<()> {
        let root = temp_dir("conflict");
        let destination = root.join("nested/out.txt");
        fs::create_dir_all(destination.parent().expect("parent"))?;
        fs::write(&destination, b"original")?;

        let mut stager = CopyStager::new();
        let stage = stager.stage(&destination)?;
        fs::write(stage.temp_path(), b"replacement")?;

        let temp_dir = stage.temp_dir().to_path_buf();
        let outcome = stage.finalize(&destination, false)?;
        assert!(matches!(
            outcome,
            CopyFinalizeOutcome::SkippedConflict { .. }
        ));
        assert_eq!(fs::read(&destination)?, b"original");
        assert!(!temp_dir.exists());
        fs::remove_dir_all(&root).ok();
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn finalize_replaces_existing_file_on_unix() -> Result<()> {
        let root = temp_dir("overwrite");
        let destination = root.join("nested/out.txt");
        fs::create_dir_all(destination.parent().expect("parent"))?;
        fs::write(&destination, b"original")?;

        let mut stager = CopyStager::new();
        let stage = stager.stage(&destination)?;
        fs::write(stage.temp_path(), b"replacement")?;

        let temp_dir = stage.temp_dir().to_path_buf();
        let outcome = stage.finalize(&destination, true)?;
        assert_eq!(outcome, CopyFinalizeOutcome::Committed);
        assert_eq!(fs::read(&destination)?, b"replacement");
        assert!(!temp_dir.exists());
        fs::remove_dir_all(&root).ok();
        Ok(())
    }
}
