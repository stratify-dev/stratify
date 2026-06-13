use stratify_core::{Confidence, Finding, IrGraph, Severity, SymbolKind};

/// Flag functions whose cyclomatic complexity exceeds `threshold`.
/// At or above 2x the threshold the finding is a Warning, otherwise Info.
pub fn analyze(graph: &IrGraph, threshold: u32) -> Vec<Finding> {
    let mut findings = Vec::new();
    for s in graph.symbols() {
        if !matches!(s.kind, SymbolKind::Function) {
            continue;
        }
        let Some(cx) = graph.complexity_of(s.id) else {
            continue;
        };
        if cx <= threshold {
            continue;
        }
        let severity = if cx >= threshold.saturating_mul(2) {
            Severity::Warning
        } else {
            Severity::Info
        };
        findings.push(Finding {
            rule: "complexity".into(),
            severity,
            message: format!(
                "function `{}` has high cyclomatic complexity ({cx})",
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

    fn func(g: &mut IrGraph, name: &str, cx: u32) -> SymbolId {
        let id = g.add_symbol(Symbol {
            id: SymbolId(0),
            kind: SymbolKind::Function,
            name: name.into(),
            fqn: name.into(),
            span: Span { file: "T.rb".into(), start_byte: 0, end_byte: 1, start_line: 1 },
            visibility: Visibility::Unknown,
            confidence: Confidence::Certain,
        });
        g.set_complexity(id, cx);
        id
    }

    #[test]
    fn flags_function_above_threshold() {
        let mut g = IrGraph::new();
        func(&mut g, "simple", 3);
        func(&mut g, "gnarly", 12);
        let findings = analyze(&g, 10);
        assert_eq!(findings.len(), 1);
        assert!(findings[0].message.contains("gnarly"));
        assert!(findings[0].message.contains("12"));
    }

    #[test]
    fn severity_escalates_at_double_threshold() {
        let mut g = IrGraph::new();
        func(&mut g, "high", 15); // > 10 but < 20 -> Info
        func(&mut g, "extreme", 25); // >= 20 -> Warning
        let findings = analyze(&g, 10);
        let high = findings.iter().find(|f| f.message.contains("high")).unwrap();
        let extreme = findings.iter().find(|f| f.message.contains("extreme")).unwrap();
        assert_eq!(high.severity, Severity::Info);
        assert_eq!(extreme.severity, Severity::Warning);
    }

    #[test]
    fn nothing_at_or_below_threshold() {
        let mut g = IrGraph::new();
        func(&mut g, "ok", 10);
        assert!(analyze(&g, 10).is_empty());
    }
}
