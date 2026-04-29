use std::collections::{BTreeMap, BTreeSet, HashMap};

#[cfg(not(test))]
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(test), derive(Serialize, Deserialize))]
pub struct ArchiveMemberRow {
    pub path: String,
    pub depth: usize,
    pub payload_family: String,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(not(test), derive(Serialize, Deserialize))]
pub struct OverlapScores {
    pub exact_overlap_score: f64,
    pub nested_overlap_score: f64,
    pub depth_weighted_overlap_score: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(not(test), derive(Serialize, Deserialize))]
pub struct ConflictBuckets {
    pub exact_path_overlap: Vec<String>,
    pub nested_path_overlap: Vec<PathPair>,
    pub path_payload_conflict: Vec<String>,
    pub payload_family_only_overlap: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Ord, PartialOrd)]
#[cfg_attr(not(test), derive(Serialize, Deserialize))]
pub struct PathPair {
    pub lhs_path: String,
    pub rhs_path: String,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(not(test), derive(Serialize, Deserialize))]
pub struct ArchiveCompareReport {
    pub left_count: usize,
    pub right_count: usize,
    pub scores: OverlapScores,
    pub buckets: ConflictBuckets,
}

pub fn compare_archive_rows<L, R>(left: L, right: R) -> ArchiveCompareReport
where
    L: IntoIterator<Item = ArchiveMemberRow>,
    R: IntoIterator<Item = ArchiveMemberRow>,
{
    let left_rows: Vec<ArchiveMemberRow> = left.into_iter().collect();
    let right_rows: Vec<ArchiveMemberRow> = right.into_iter().collect();

    let left_paths: BTreeSet<String> = left_rows.iter().map(|r| r.path.clone()).collect();
    let right_paths: BTreeSet<String> = right_rows.iter().map(|r| r.path.clone()).collect();

    let exact_paths: BTreeSet<String> = left_paths.intersection(&right_paths).cloned().collect();
    let union_paths: BTreeSet<String> = left_paths.union(&right_paths).cloned().collect();

    let nested_pairs = collect_nested_pairs(&left_paths, &right_paths, &exact_paths);
    let nested_unique_left: BTreeSet<String> = nested_pairs
        .iter()
        .map(|pair| pair.lhs_path.clone())
        .collect();
    let nested_unique_right: BTreeSet<String> = nested_pairs
        .iter()
        .map(|pair| pair.rhs_path.clone())
        .collect();

    let union_count = union_paths.len();
    let exact_score = ratio(exact_paths.len(), union_count);
    let nested_match_units =
        exact_paths.len() + nested_unique_left.len().min(nested_unique_right.len());
    let nested_score = ratio(nested_match_units, union_count);

    let depth_weighted_score =
        depth_weighted_overlap(&left_rows, &right_rows, &exact_paths, &union_paths);

    let path_payload_conflict =
        collect_path_payload_conflicts(&left_rows, &right_rows, &exact_paths);
    let payload_family_only_overlap =
        collect_payload_family_only_overlap(&left_rows, &right_rows, &exact_paths);

    ArchiveCompareReport {
        left_count: left_rows.len(),
        right_count: right_rows.len(),
        scores: OverlapScores {
            exact_overlap_score: exact_score,
            nested_overlap_score: nested_score,
            depth_weighted_overlap_score: depth_weighted_score,
        },
        buckets: ConflictBuckets {
            exact_path_overlap: exact_paths.into_iter().collect(),
            nested_path_overlap: nested_pairs.into_iter().collect(),
            path_payload_conflict,
            payload_family_only_overlap,
        },
    }
}

fn collect_nested_pairs(
    left_paths: &BTreeSet<String>,
    right_paths: &BTreeSet<String>,
    exact_paths: &BTreeSet<String>,
) -> BTreeSet<PathPair> {
    let mut pairs = BTreeSet::new();

    // O(n^2) pair scans do not scale for large archive-member sets.
    // Use strict ancestor-prefix lookups instead: O(paths * depth * log n).
    for lhs in left_paths {
        if exact_paths.contains(lhs) {
            continue;
        }
        for rhs_ancestor in strict_parent_prefixes(lhs) {
            if right_paths.contains(rhs_ancestor) {
                pairs.insert(PathPair {
                    lhs_path: lhs.clone(),
                    rhs_path: rhs_ancestor.to_string(),
                });
            }
        }
    }

    for rhs in right_paths {
        if exact_paths.contains(rhs) {
            continue;
        }
        for lhs_ancestor in strict_parent_prefixes(rhs) {
            if left_paths.contains(lhs_ancestor) {
                pairs.insert(PathPair {
                    lhs_path: lhs_ancestor.to_string(),
                    rhs_path: rhs.clone(),
                });
            }
        }
    }

    pairs
}

fn strict_parent_prefixes(path: &str) -> Vec<&str> {
    let mut out = Vec::new();
    for (idx, ch) in path.char_indices() {
        if ch == '/' && idx > 0 {
            out.push(&path[..idx]);
        }
    }
    out
}

fn depth_weighted_overlap(
    left_rows: &[ArchiveMemberRow],
    right_rows: &[ArchiveMemberRow],
    exact_paths: &BTreeSet<String>,
    union_paths: &BTreeSet<String>,
) -> f64 {
    if union_paths.is_empty() {
        return 0.0;
    }

    let left_depth_by_path = max_depth_by_path(left_rows);
    let right_depth_by_path = max_depth_by_path(right_rows);

    let exact_weight: usize = exact_paths
        .iter()
        .map(|path| {
            left_depth_by_path
                .get(path)
                .copied()
                .unwrap_or(0)
                .max(right_depth_by_path.get(path).copied().unwrap_or(0))
                + 1
        })
        .sum();

    let union_weight: usize = union_paths
        .iter()
        .map(|path| {
            left_depth_by_path
                .get(path)
                .copied()
                .unwrap_or(0)
                .max(right_depth_by_path.get(path).copied().unwrap_or(0))
                + 1
        })
        .sum();

    ratio(exact_weight, union_weight)
}

fn max_depth_by_path(rows: &[ArchiveMemberRow]) -> HashMap<String, usize> {
    let mut map: HashMap<String, usize> = HashMap::new();
    for row in rows {
        map.entry(row.path.clone())
            .and_modify(|d| *d = (*d).max(row.depth))
            .or_insert(row.depth);
    }
    map
}

fn collect_path_payload_conflicts(
    left_rows: &[ArchiveMemberRow],
    right_rows: &[ArchiveMemberRow],
    exact_paths: &BTreeSet<String>,
) -> Vec<String> {
    let left_family = family_set_by_path(left_rows);
    let right_family = family_set_by_path(right_rows);
    let mut conflicts = BTreeSet::new();

    for path in exact_paths {
        let lf = left_family.get(path);
        let rf = right_family.get(path);
        if lf != rf {
            conflicts.insert(path.clone());
        }
    }

    conflicts.into_iter().collect()
}

fn collect_payload_family_only_overlap(
    left_rows: &[ArchiveMemberRow],
    right_rows: &[ArchiveMemberRow],
    exact_paths: &BTreeSet<String>,
) -> Vec<String> {
    let left_family_by_path = family_set_by_path(left_rows);
    let right_family_by_path = family_set_by_path(right_rows);

    let left_families: BTreeSet<String> =
        left_rows.iter().map(|r| r.payload_family.clone()).collect();
    let right_families: BTreeSet<String> = right_rows
        .iter()
        .map(|r| r.payload_family.clone())
        .collect();

    let mut families = BTreeSet::new();
    for family in left_families.intersection(&right_families) {
        let has_exact_on_same_family = exact_paths.iter().any(|path| {
            let lf = left_family_by_path.get(path);
            let rf = right_family_by_path.get(path);
            match (lf, rf) {
                (Some(l), Some(r)) => l.contains(family) && r.contains(family),
                _ => false,
            }
        });
        if !has_exact_on_same_family {
            families.insert(family.clone());
        }
    }

    families.into_iter().collect()
}

fn family_set_by_path(rows: &[ArchiveMemberRow]) -> BTreeMap<String, BTreeSet<String>> {
    let mut map = BTreeMap::new();
    for row in rows {
        map.entry(row.path.clone())
            .or_insert_with(BTreeSet::new)
            .insert(row.payload_family.clone());
    }
    map
}

fn ratio(num: usize, den: usize) -> f64 {
    if den == 0 {
        0.0
    } else {
        num as f64 / den as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(path: &str, depth: usize, payload_family: &str) -> ArchiveMemberRow {
        ArchiveMemberRow {
            path: path.to_string(),
            depth,
            payload_family: payload_family.to_string(),
        }
    }

    #[test]
    fn exact_overlap_is_full_and_deterministic() {
        let left = vec![row("a/b.txt", 2, "text"), row("c/d.bin", 2, "binary")];
        let right = vec![row("a/b.txt", 2, "text"), row("c/d.bin", 2, "binary")];

        let report = compare_archive_rows(left, right);
        assert_eq!(report.scores.exact_overlap_score, 1.0);
        assert_eq!(report.scores.nested_overlap_score, 1.0);
        assert_eq!(report.scores.depth_weighted_overlap_score, 1.0);
        assert_eq!(
            report.buckets.exact_path_overlap,
            vec!["a/b.txt".to_string(), "c/d.bin".to_string()]
        );
        assert!(report.buckets.nested_path_overlap.is_empty());
        assert!(report.buckets.path_payload_conflict.is_empty());
        assert!(report.buckets.payload_family_only_overlap.is_empty());
    }

    #[test]
    fn depth_weighted_overlap_reflects_depth_skew() {
        let left = vec![row("flat.txt", 1, "text"), row("a/b/c/d.bin", 4, "binary")];
        let right = vec![row("flat.txt", 1, "text"), row("x/y/z/q.bin", 4, "binary")];

        let report = compare_archive_rows(left, right);

        assert_eq!(report.scores.exact_overlap_score, 1.0 / 3.0);
        assert_eq!(report.scores.depth_weighted_overlap_score, 2.0 / 12.0);
        assert!(report.scores.depth_weighted_overlap_score < report.scores.exact_overlap_score);
    }

    #[test]
    fn payload_family_only_overlap_detected_without_path_overlap() {
        let left = vec![
            row("left/a.json", 2, "json"),
            row("left/b.bin", 2, "binary"),
        ];
        let right = vec![
            row("right/c.json", 2, "json"),
            row("right/d.txt", 2, "text"),
        ];

        let report = compare_archive_rows(left, right);

        assert_eq!(report.scores.exact_overlap_score, 0.0);
        assert_eq!(
            report.buckets.payload_family_only_overlap,
            vec!["json".to_string()]
        );
        assert!(report.buckets.exact_path_overlap.is_empty());
        assert!(report.buckets.path_payload_conflict.is_empty());
    }

    #[test]
    fn nested_pairs_are_detected_without_quadratic_scan_behavior() {
        let left = vec![
            row("root/a", 2, "text"),
            row("root/a/deeper/item.bin", 4, "binary"),
            row("x/y/z", 3, "text"),
        ];
        let right = vec![
            row("root", 1, "text"),
            row("x/y", 2, "text"),
            row("other/path", 2, "text"),
        ];

        let report = compare_archive_rows(left, right);
        let pairs = report.buckets.nested_path_overlap;
        assert!(pairs.contains(&PathPair {
            lhs_path: "root/a".to_string(),
            rhs_path: "root".to_string()
        }));
        assert!(pairs.contains(&PathPair {
            lhs_path: "root/a/deeper/item.bin".to_string(),
            rhs_path: "root".to_string()
        }));
        assert!(pairs.contains(&PathPair {
            lhs_path: "x/y/z".to_string(),
            rhs_path: "x/y".to_string()
        }));
    }
}
