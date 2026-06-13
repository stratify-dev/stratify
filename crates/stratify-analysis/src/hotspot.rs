use std::collections::HashMap;
use stratify_core::{Confidence, Finding, IrGraph, Severity, SymbolKind};

/// Hotspot = function complexity x churn of its file. Flags functions whose
/// score exceeds `threshold`. Churn is supplied by the caller (the CLI reads
/// it from git), keyed by the same file string the IR uses in spans.
pub fn analyze(
    graph: &IrGraph,
    churn: &HashMap<String, u32>,
    threshold: u32,
) -> Vec<Finding> {
    let mut findings = Vec::new();
    for s in graph.symbols() {
        if !matches!(s.kind, SymbolKind::Function) {
            continue;
        }
        let Some(cx) = graph.complexity_of(s.id) else {
            continue;
        };
        let ch = churn.get(&s.span.file).copied().unwrap_or(0);
        let score = cx.saturating_mul(ch);
        if score <= threshold {
            continue;
        }
        findings.push(Finding {
            rule: "hotspot".into(),
            severity: Severity::Warning,
            message: format!(
                "hotspot: `{}` complexity {cx} x churn {ch} = {score}",
                s.name
            ),
            span: s.span.clone(),
            confidence: Confidence::Certain,
        });
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use stratify_core::ir::{Span, Symbol, SymbolId, Visibility};

    fn func(g: &mut IrGraph, name: &str, file: &str, cx: u32) -> SymbolId {
        let id = g.add_symbol(Symbol {
            id: SymbolId(0),
            kind: SymbolKind::Function,
            name: name.into(),
            fqn: name.into(),
            span: Span { file: file.into(), start_byte: 0, end_byte: 1, start_line: 1 },
            visibility: Visibility::Unknown,
            confidence: Confidence::Certain,
        });
        g.set_complexity(id, cx);
        id
    }

    #[test]
    fn flags_complex_and_churny() {
        let mut g = IrGraph::new();
        func(&mut g, "hot", "a.rb", 11);
        func(&mut g, "calm", "b.rb", 11);
        let mut churn = HashMap::new();
        churn.insert("a.rb".to_string(), 6); // 11*6 = 66 > 50
        churn.insert("b.rb".to_string(), 1); // 11*1 = 11 <= 50
        let findings = analyze(&g, &churn, 50);
        assert_eq!(findings.len(), 1);
        assert!(findings[0].message.contains("hot"));
        assert!(findings[0].message.contains("66"));
    }

    #[test]
    fn no_hotspot_without_churn() {
        let mut g = IrGraph::new();
        func(&mut g, "complex", "a.rb", 30);
        let churn = HashMap::new(); // no churn data -> score 0
        assert!(analyze(&g, &churn, 50).is_empty());
    }

    #[test]
    fn function_without_complexity_is_skipped() {
        let mut g = IrGraph::new();
        // a function with no recorded complexity
        g.add_symbol(Symbol {
            id: SymbolId(0),
            kind: SymbolKind::Function,
            name: "x".into(),
            fqn: "x".into(),
            span: Span { file: "a.rb".into(), start_byte: 0, end_byte: 1, start_line: 1 },
            visibility: Visibility::Unknown,
            confidence: Confidence::Certain,
        });
        let mut churn = HashMap::new();
        churn.insert("a.rb".to_string(), 100);
        assert!(analyze(&g, &churn, 50).is_empty());
    }
}
