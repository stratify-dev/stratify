use std::collections::HashMap;
use stratify_core::ir::Span;
use stratify_core::{Confidence, Finding, IrGraph, Severity};

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

    // Emit one finding per left-maximal duplicated region.
    let mut findings = Vec::new();
    for s in 0..duplicated.len() {
        if duplicated[s] && (s == 0 || !duplicated[s - 1]) {
            let group = &groups[&ids[s..s + k]];
            if let Some(&other) = group.iter().find(|&&o| o != s) {
                let here = &tokens[s];
                let there = &tokens[other];
                let last = &tokens[s + k - 1];
                findings.push(Finding {
                    rule: "duplication".into(),
                    severity: Severity::Warning,
                    message: format!(
                        "duplicated code block (>= {k} tokens) also at {}:{}",
                        there.file, there.start_line
                    ),
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
        assert!(findings.iter().any(|f| f.span.file == "a.rb" && f.message.contains("b.rb")));
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
    fn straddling_window_is_not_a_clone() {
        // a.rb + b.rb tokens concatenated happen to equal c.rb's content,
        // but that is a boundary artifact, not a real clone. Must report nothing.
        let mut g = IrGraph::new();
        push(&mut g, "a.rb", &["ID", "=", "NUM"], 1);   // 3 tokens
        push(&mut g, "b.rb", &["+", "ID"], 1);          // 2 tokens
        push(&mut g, "c.rb", &["ID", "=", "NUM", "+", "ID"], 1); // 5 tokens, single file
        assert!(analyze(&g, 5).is_empty(), "boundary straddle must not be a clone");
    }
}
