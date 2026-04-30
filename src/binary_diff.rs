use std::cmp;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

const DEFAULT_WINDOW_SIZE: usize = 4096;
const DEFAULT_MAX_WINDOWS: usize = 24;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BinaryDiffConfig {
    pub window_size: usize,
    pub max_windows: usize,
}

impl Default for BinaryDiffConfig {
    fn default() -> Self {
        Self {
            window_size: DEFAULT_WINDOW_SIZE,
            max_windows: DEFAULT_MAX_WINDOWS,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinaryDigest {
    pub size: u64,
    pub full_hash: String,
    pub head_hash: String,
    pub tail_hash: String,
    pub sample_hashes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BinaryDiffResult {
    pub similarity: f64,
    pub aligned_similarity: f64,
    pub shifted_alignment_similarity: f64,
    pub content_similarity: f64,
    pub histogram_similarity: f64,
    pub windows_compared: usize,
    pub matching_aligned_windows: usize,
    pub left: BinaryDigest,
    pub right: BinaryDigest,
}

pub fn diff_files<P: AsRef<Path>>(
    left_path: P,
    right_path: P,
    config: BinaryDiffConfig,
) -> io::Result<BinaryDiffResult> {
    let mut left = File::open(left_path)?;
    let mut right = File::open(right_path)?;

    let left_len = left.metadata()?.len();
    let right_len = right.metadata()?.len();

    let left_windows = sample_hashes(&mut left, left_len, config)?;
    let right_windows = sample_hashes(&mut right, right_len, config)?;

    let windows_compared = cmp::max(left_windows.len(), right_windows.len());
    let zipped = cmp::min(left_windows.len(), right_windows.len());

    let matching_aligned_windows = left_windows
        .iter()
        .zip(right_windows.iter())
        .filter(|(l, r)| l == r)
        .count();

    let aligned_similarity = if zipped == 0 {
        if left_len == 0 && right_len == 0 {
            1.0
        } else {
            0.0
        }
    } else {
        matching_aligned_windows as f64 / zipped as f64
    };
    let shifted_alignment_similarity =
        best_cyclic_alignment_similarity(&left_windows, &right_windows);

    let content_similarity = multiset_jaccard(&left_windows, &right_windows);
    let histogram_similarity = histogram_similarity(&mut left, &mut right)?;
    let size_similarity = size_similarity(left_len, right_len);
    let similarity = ((0.35 * content_similarity)
        + (0.15 * aligned_similarity)
        + (0.35 * histogram_similarity)
        + (0.1 * shifted_alignment_similarity)
        + (0.05 * size_similarity))
        .clamp(0.0, 1.0);

    let left_digest = digest_for_file(&mut left, left_len, &left_windows, config)?;
    let right_digest = digest_for_file(&mut right, right_len, &right_windows, config)?;

    Ok(BinaryDiffResult {
        similarity,
        aligned_similarity,
        shifted_alignment_similarity,
        content_similarity,
        histogram_similarity,
        windows_compared,
        matching_aligned_windows,
        left: left_digest,
        right: right_digest,
    })
}

fn histogram_similarity(left: &mut File, right: &mut File) -> io::Result<f64> {
    let left_hist = byte_histogram(left)?;
    let right_hist = byte_histogram(right)?;

    let mut intersection = 0u64;
    let mut union = 0u64;
    for i in 0..256 {
        intersection += cmp::min(left_hist[i], right_hist[i]);
        union += cmp::max(left_hist[i], right_hist[i]);
    }

    if union == 0 {
        Ok(1.0)
    } else {
        Ok(intersection as f64 / union as f64)
    }
}

fn byte_histogram(file: &mut File) -> io::Result<[u64; 256]> {
    file.seek(SeekFrom::Start(0))?;
    let mut hist = [0_u64; 256];
    let mut buf = [0_u8; 8192];

    loop {
        let read = file.read(&mut buf)?;
        if read == 0 {
            break;
        }
        for b in &buf[..read] {
            hist[*b as usize] += 1;
        }
    }

    Ok(hist)
}

fn best_cyclic_alignment_similarity(left: &[u64], right: &[u64]) -> f64 {
    let n = cmp::min(left.len(), right.len());
    if n == 0 {
        return if left.is_empty() && right.is_empty() {
            1.0
        } else {
            0.0
        };
    }

    let left = &left[..n];
    let right = &right[..n];
    let mut best = 0usize;

    for shift in 0..n {
        let mut matches = 0usize;
        for i in 0..n {
            if left[i] == right[(i + shift) % n] {
                matches += 1;
            }
        }
        best = cmp::max(best, matches);
    }

    best as f64 / n as f64
}

fn sample_hashes(file: &mut File, len: u64, config: BinaryDiffConfig) -> io::Result<Vec<u64>> {
    if len == 0 {
        return Ok(Vec::new());
    }

    let window_size = effective_window_size(len, config.window_size);
    let offsets = sample_offsets(len, window_size as u64, config.max_windows.max(1));
    let mut out = Vec::with_capacity(offsets.len());
    let mut buffer = vec![0_u8; window_size];

    for offset in offsets {
        let read = read_window(file, offset, &mut buffer)?;
        out.push(fnv1a64(&buffer[..read]));
    }

    Ok(out)
}

fn digest_for_file(
    file: &mut File,
    len: u64,
    sampled_hashes: &[u64],
    config: BinaryDiffConfig,
) -> io::Result<BinaryDigest> {
    let full_hash = hash_entire_file(file)?;
    let head_hash = hash_region(file, 0, cmp::min(len, config.window_size as u64))?;

    let tail_len = cmp::min(len, config.window_size as u64);
    let tail_start = len.saturating_sub(tail_len);
    let tail_hash = hash_region(file, tail_start, tail_len)?;

    let sample_hashes = sampled_hashes
        .iter()
        .map(|h| to_hex64(*h))
        .collect::<Vec<_>>();

    Ok(BinaryDigest {
        size: len,
        full_hash: to_hex64(full_hash),
        head_hash: to_hex64(head_hash),
        tail_hash: to_hex64(tail_hash),
        sample_hashes,
    })
}

fn hash_entire_file(file: &mut File) -> io::Result<u64> {
    file.seek(SeekFrom::Start(0))?;
    let mut hasher = Fnv1a64::new();
    let mut buf = [0_u8; 8192];
    loop {
        let read = file.read(&mut buf)?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(hasher.finish())
}

fn hash_region(file: &mut File, offset: u64, len: u64) -> io::Result<u64> {
    file.seek(SeekFrom::Start(offset))?;
    let mut remaining = len;
    let mut hasher = Fnv1a64::new();
    let mut buf = [0_u8; 8192];

    while remaining > 0 {
        let take = cmp::min(remaining, buf.len() as u64) as usize;
        let read = file.read(&mut buf[..take])?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
        remaining -= read as u64;
    }

    Ok(hasher.finish())
}

fn read_window(file: &mut File, offset: u64, buffer: &mut [u8]) -> io::Result<usize> {
    file.seek(SeekFrom::Start(offset))?;
    let mut filled = 0;
    while filled < buffer.len() {
        let read = file.read(&mut buffer[filled..])?;
        if read == 0 {
            break;
        }
        filled += read;
    }
    Ok(filled)
}

fn sample_offsets(len: u64, window: u64, max_windows: usize) -> Vec<u64> {
    if len == 0 || window == 0 {
        return Vec::new();
    }
    if len <= window {
        return vec![0];
    }

    let limit = len - window;
    let target = max_windows.max(2);
    let mut offsets = Vec::with_capacity(target);

    offsets.push(0);
    if target > 2 {
        for i in 1..(target - 1) {
            let numerator = (i as u128) * (limit as u128);
            let denominator = (target as u128) - 1;
            offsets.push((numerator / denominator) as u64);
        }
    }
    offsets.push(limit);

    offsets.sort_unstable();
    offsets.dedup();
    offsets
}

fn effective_window_size(len: u64, requested: usize) -> usize {
    let requested = requested.max(1);
    cmp::min(requested, len as usize)
}

fn multiset_jaccard(left: &[u64], right: &[u64]) -> f64 {
    if left.is_empty() && right.is_empty() {
        return 1.0;
    }

    let mut l = BTreeMap::<u64, usize>::new();
    let mut r = BTreeMap::<u64, usize>::new();
    for h in left {
        *l.entry(*h).or_insert(0) += 1;
    }
    for h in right {
        *r.entry(*h).or_insert(0) += 1;
    }

    let mut intersection = 0usize;
    let mut union = 0usize;

    for (k, lv) in &l {
        let rv = r.get(k).copied().unwrap_or(0);
        intersection += cmp::min(*lv, rv);
        union += cmp::max(*lv, rv);
    }

    for (k, rv) in &r {
        if !l.contains_key(k) {
            union += *rv;
        }
    }

    if union == 0 {
        1.0
    } else {
        intersection as f64 / union as f64
    }
}

fn size_similarity(left: u64, right: u64) -> f64 {
    if left == 0 && right == 0 {
        return 1.0;
    }
    let min = cmp::min(left, right) as f64;
    let max = cmp::max(left, right) as f64;
    if max == 0.0 { 1.0 } else { min / max }
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h = Fnv1a64::new();
    h.update(bytes);
    h.finish()
}

fn to_hex64(v: u64) -> String {
    format!("{v:016x}")
}

#[derive(Clone, Copy)]
struct Fnv1a64 {
    state: u64,
}

impl Fnv1a64 {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;

    fn new() -> Self {
        Self {
            state: Self::OFFSET,
        }
    }

    fn update(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.state ^= u64::from(*b);
            self.state = self.state.wrapping_mul(Self::PRIME);
        }
    }

    fn finish(self) -> u64 {
        self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_file_path(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        std::env::temp_dir().join(format!("nightindex-{prefix}-{nanos}.bin"))
    }

    fn write_temp(contents: &[u8], prefix: &str) -> PathBuf {
        let p = temp_file_path(prefix);
        fs::write(&p, contents).expect("write temp file");
        p
    }

    #[test]
    fn identical_files_score_one() {
        let data = (0..64_000u32).map(|v| (v % 251) as u8).collect::<Vec<_>>();
        let left = write_temp(&data, "identical-left");
        let right = write_temp(&data, "identical-right");

        let result = diff_files(&left, &right, BinaryDiffConfig::default()).expect("diff");
        fs::remove_file(left).expect("cleanup left");
        fs::remove_file(right).expect("cleanup right");

        assert!((result.similarity - 1.0).abs() < f64::EPSILON);
        assert_eq!(result.aligned_similarity, 1.0);
        assert_eq!(result.shifted_alignment_similarity, 1.0);
        assert_eq!(result.content_similarity, 1.0);
        assert_eq!(result.histogram_similarity, 1.0);
        assert_eq!(result.left.full_hash, result.right.full_hash);
    }

    #[test]
    fn shifted_blocks_retains_nontrivial_similarity() {
        let mut base = Vec::with_capacity(192_000);
        let mut x = 0x1234_5678_9abc_def0_u64;
        for _ in 0..192_000 {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
            base.push((x >> 24) as u8);
        }

        let mut shifted = Vec::new();
        shifted.extend_from_slice(&base[8192..]);
        shifted.extend_from_slice(&base[..8192]);

        let left = write_temp(&base, "shift-left");
        let right = write_temp(&shifted, "shift-right");

        let cfg = BinaryDiffConfig {
            window_size: 1024,
            max_windows: 40,
        };
        let result = diff_files(&left, &right, cfg).expect("diff");
        fs::remove_file(left).expect("cleanup left");
        fs::remove_file(right).expect("cleanup right");

        assert!(result.aligned_similarity < 1.0, "{:?}", result);
        assert!(
            result.shifted_alignment_similarity >= result.aligned_similarity,
            "{:?}",
            result
        );
        assert!(result.histogram_similarity > 0.95, "{:?}", result);
        assert!(result.similarity > 0.30, "{:?}", result);
    }

    #[test]
    fn tiny_files_are_handled_safely() {
        let left = write_temp(&[1, 2, 3], "tiny-left");
        let right = write_temp(&[1, 2, 4], "tiny-right");

        let cfg = BinaryDiffConfig {
            window_size: 4096,
            max_windows: 16,
        };
        let result = diff_files(&left, &right, cfg).expect("diff");
        fs::remove_file(left).expect("cleanup left");
        fs::remove_file(right).expect("cleanup right");

        assert_eq!(result.windows_compared, 1);
        assert_eq!(result.left.sample_hashes.len(), 1);
        assert_eq!(result.right.sample_hashes.len(), 1);
        assert!(result.similarity >= 0.0 && result.similarity <= 1.0);
        assert_ne!(result.left.full_hash, result.right.full_hash);
    }
}
