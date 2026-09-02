use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use stratify_core::ir::Span;
use stratify_core::{Confidence, Finding, IrGraph, Severity};

/// The `[duplication]` table of `stratify.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DuplicationSection {
    /// Minimum identical normalized-token run length to count as a clone.
    /// Absent means use the caller's default.
    #[serde(default)]
    pub min_tokens: Option<usize>,
}

/// Wrapper to deserialize just the `[duplication]` table from `stratify.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DuplicationToml {
    #[serde(default)]
    pub duplication: DuplicationSection,
}

/// Detect duplicated code as identical windows of `min_tokens` normalized
/// tokens. Reports one finding per left-maximal duplicated region, pointing at
/// another copy. Exact token-sequence match, so confidence is Certain.
pub fn analyze(graph: &IrGraph, min_tokens: usize) -> Vec<Finding> {
    let tokens = graph.tokens();
    let n = tokens.len();
    let k = min_tokens;
    if k == 0 || n < k {
        return Vec::new();
    }

    // Intern normalized token text to dense u32 ids.
    let mut interner: HashMap<&str, u32> = HashMap::new();
    let mut ids: Vec<u32> = Vec::with_capacity(n);
    for t in tokens {
        let next = interner.len() as u32;
        let id = *interner.entry(t.norm.as_str()).or_insert(next);
        ids.push(id);
    }

    // Group identical k-token windows by their exact content.
    let mut groups: HashMap<&[u32], Vec<usize>> = HashMap::new();
    for s in 0..=(n - k) {
        // Skip windows that straddle a file boundary. Per-file tokens are
        // contiguous in the stream, so checking the endpoints is sufficient.
        if tokens[s].file != tokens[s + k - 1].file {
            continue;
        }
        groups.entry(&ids[s..s + k]).or_default().push(s);
    }

    // duplicated[s] = the window starting at s appears at >= 2 positions.
    let mut duplicated = vec![false; n - k + 1];
    for starts in groups.values() {
        if starts.len() >= 2 {
            for &s in starts {
                duplicated[s] = true;
            }
        }
    }

    // Emit one finding per clone cluster, anchored at the earliest occurrence.
    let mut findings = Vec::new();
    for s in 0..duplicated.len() {
        if duplicated[s] && (s == 0 || !duplicated[s - 1]) {
            let group = &groups[&ids[s..s + k]];

            // B2: exclude same-file occurrences reachable from `s` through a
            // chain of near neighbors (each hop < k). A repetitive ladder
            // (many near-identical branches in a row) produces exactly this
            // shape: consecutive branches sit well within one window of each
            // other, but a long enough ladder drifts the first and last
            // occurrence past k tokens apart. Comparing only the pairwise
            // distance from `s` missed that case - the whole chain is one
            // self-similar structure, not a copy elsewhere, so it has to be
            // suppressed as a connected component, not member-by-member.
            let mut same_file: Vec<usize> = group
                .iter()
                .copied()
                .filter(|&o| tokens[o].file == tokens[s].file)
                .collect();
            same_file.sort_unstable();
            let s_idx = same_file.binary_search(&s).unwrap();
            let mut lo = s_idx;
            while lo > 0 && same_file[lo] - same_file[lo - 1] < k {
                lo -= 1;
            }
            let mut hi = s_idx;
            while hi + 1 < same_file.len() && same_file[hi + 1] - same_file[hi] < k {
                hi += 1;
            }
            let self_chain: HashSet<usize> = same_file[lo..=hi].iter().copied().collect();

            let valid_others: Vec<usize> = group
                .iter()
                .copied()
                .filter(|&o| o != s && !self_chain.contains(&o))
                .collect();

            // No actionable copy remains: drop the finding.
            if valid_others.is_empty() {
                continue;
            }

            // B1: a cluster reports exactly once, from its earliest occurrence.
            if s != *group.iter().min().unwrap() {
                continue;
            }

            // Point at the nearest valid other copy.
            let other = *valid_others
                .iter()
                .min_by_key(|&&o| (o as isize - s as isize).unsigned_abs())
                .unwrap();
            let here = &tokens[s];
            let there = &tokens[other];
            let last = &tokens[s + k - 1];
            let mut message = format!(
                "duplicated code block (>= {k} tokens) also at {}:{}",
                there.file, there.start_line
            );
            if valid_others.len() > 1 {
                message.push_str(&format!(" and {} more", valid_others.len() - 1));
            }
            findings.push(Finding {
                rule: "duplication".into(),
                severity: Severity::Warning,
                message,
                span: Span {
                    file: here.file.clone(),
                    start_byte: here.start_byte,
                    end_byte: last.end_byte,
                    start_line: here.start_line,
                },
                confidence: Confidence::Certain,
            });
        }
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use stratify_core::ir::Token;

    fn push(g: &mut IrGraph, file: &str, norms: &[&str], base_line: usize) {
        for (i, nrm) in norms.iter().enumerate() {
            g.add_token(Token {
                file: file.into(),
                start_byte: i,
                end_byte: i + 1,
                start_line: base_line + i,
                norm: (*nrm).into(),
            });
        }
    }

    #[test]
    fn finds_a_cross_file_clone() {
        let mut g = IrGraph::new();
        let block = ["ID", "=", "ID", "+", "NUM", "ID"];
        push(&mut g, "a.rb", &block, 10);
        push(&mut g, "b.rb", &block, 20);
        let findings = analyze(&g, 5);
        assert!(!findings.is_empty());
        assert_eq!(findings[0].rule, "duplication");
        // The first region is in a.rb and points at b.rb.
        assert!(findings
            .iter()
            .any(|f| f.span.file == "a.rb" && f.message.contains("b.rb")));
    }

    #[test]
    fn no_clone_when_unique() {
        let mut g = IrGraph::new();
        push(&mut g, "a.rb", &["ID", "=", "NUM"], 1);
        push(&mut g, "b.rb", &["ID", "+", "STR"], 1);
        assert!(analyze(&g, 5).is_empty());
    }

    #[test]
    fn ignores_blocks_shorter_than_min() {
        let mut g = IrGraph::new();
        let block = ["ID", "+", "ID"];
        push(&mut g, "a.rb", &block, 1);
        push(&mut g, "b.rb", &block, 1);
        // window of 5 over a 3-token block per file: each file alone is < k,
        // and the two files' tokens are not adjacent in a single 5-run, so no finding.
        assert!(analyze(&g, 5).is_empty());
    }

    #[test]
    fn two_file_clone_reports_once() {
        let mut g = IrGraph::new();
        let block = ["ID", "=", "ID", "+"];
        push(&mut g, "a.rb", &block, 10);
        push(&mut g, "b.rb", &block, 20);
        let findings = analyze(&g, 4);
        assert_eq!(findings.len(), 1, "a 2-file clone must report exactly once");
        // Earliest occurrence is in a.rb; it points at the other file.
        assert_eq!(findings[0].span.file, "a.rb");
        assert!(findings[0].message.contains("b.rb"));
    }

    #[test]
    fn three_file_clone_reports_once() {
        let mut g = IrGraph::new();
        let block = ["ID", "=", "ID", "+"];
        push(&mut g, "a.rb", &block, 10);
        push(&mut g, "b.rb", &block, 20);
        push(&mut g, "c.rb", &block, 30);
        let findings = analyze(&g, 4);
        assert_eq!(findings.len(), 1, "a 3-file clone must report exactly once");
        assert_eq!(findings[0].span.file, "a.rb");
        assert!(
            findings[0].message.contains("and 1 more"),
            "message should note the extra copy: {}",
            findings[0].message
        );
    }

    #[test]
    fn overlapping_self_match_suppressed() {
        // Repetitive ladder: a single token repeated so identical 4-token
        // windows recur only at overlapping offsets (< k) within one file.
        // With 7 copies the window at 0 also matches at 1,2,3 (all < k), and
        // no recurrence lands >= k away, so every match is an overlap.
        let mut g = IrGraph::new();
        let stream = ["LADDER"; 7];
        push(&mut g, "x.rb", &stream, 1);
        let findings = analyze(&g, 4);
        assert_eq!(
            findings.len(),
            0,
            "overlapping same-file self-matches must be suppressed"
        );
    }

    #[test]
    fn long_ladder_self_chain_fully_suppressed() {
        // A longer repetitive ladder than `overlapping_self_match_suppressed`
        // covers: period-3 branches, repeated 8 times (24 tokens), k=10.
        // With valid window starts 0..=14, the phase-0 group (every position
        // whose 10-token window is byte-identical) is {0, 3, 6, 9, 12} - each
        // consecutive hop is 3, always < k, so the whole chain is one
        // connected self-similar structure. But position 0 and position 12
        // are 12 tokens apart, >= k=10, so the old single-anchor "is o within
        // k of s" check does not suppress that pair: B2's fix only ever
        // compared each far member directly against the earliest occurrence,
        // never asking whether that far member was still reachable from `s`
        // through a chain of near neighbors. This reproduces the real B2
        // regression seen independently in TypeScript and Rust 21-branch
        // if/else-if ladders (a long enough ladder drifts past the
        // token-distance guard).
        let mut g = IrGraph::new();
        let branch = ["IF", "ID", "RET"];
        let stream: Vec<&str> = branch.iter().copied().cycle().take(24).collect();
        push(&mut g, "ladder.rb", &stream, 1);
        let findings = analyze(&g, 10);
        assert_eq!(
            findings.len(),
            0,
            "a fully-connected repetitive-ladder chain must be suppressed \
             end to end, not just pairwise against the earliest occurrence: {findings:?}"
        );
    }

    #[test]
    fn nonoverlapping_in_file_dup_still_reported() {
        // Same 4-token block twice in one file, separated by >= k unique fillers.
        let mut g = IrGraph::new();
        let block = ["ID", "=", "ID", "+"];
        let filler = ["F1", "F2", "F3", "F4", "F5"];
        let mut stream: Vec<&str> = Vec::new();
        stream.extend_from_slice(&block);
        stream.extend_from_slice(&filler);
        stream.extend_from_slice(&block);
        push(&mut g, "a.rb", &stream, 1);
        let findings = analyze(&g, 4);
        assert_eq!(
            findings.len(),
            1,
            "non-overlapping in-file duplicate must report once"
        );
        assert_eq!(findings[0].span.file, "a.rb");
    }

    #[test]
    fn straddling_window_is_not_a_clone() {
        // a.rb + b.rb tokens concatenated happen to equal c.rb's content,
        // but that is a boundary artifact, not a real clone. Must report nothing.
        let mut g = IrGraph::new();
        push(&mut g, "a.rb", &["ID", "=", "NUM"], 1); // 3 tokens
        push(&mut g, "b.rb", &["+", "ID"], 1); // 2 tokens
        push(&mut g, "c.rb", &["ID", "=", "NUM", "+", "ID"], 1); // 5 tokens, single file
        assert!(
            analyze(&g, 5).is_empty(),
            "boundary straddle must not be a clone"
        );
    }
}
