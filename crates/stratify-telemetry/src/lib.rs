//! Pure mapping from a Stratify Report + scan aggregates to OTLP-ready
//! metric points and a per-run event. No network, no OTel SDK here.

use stratify_core::{Confidence, Report, Severity};

/// One gauge data point: a metric name, a value, and low-cardinality attributes.
#[derive(Debug, Clone, PartialEq)]
pub struct MetricPoint {
    pub name: String,
    pub value: f64,
    pub attributes: Vec<(String, String)>,
}

fn severity_str(s: Severity) -> &'static str {
    match s {
        Severity::Info => "info",
        Severity::Warning => "warning",
        Severity::Error => "error",
    }
}

fn confidence_str(c: Confidence) -> &'static str {
    match c {
        Confidence::Unknown => "unknown",
        Confidence::Likely => "likely",
        Confidence::Certain => "certain",
    }
}

/// Language name from a file path by extension, or "unknown".
pub fn lang_of(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("java") => "java",
        Some("rb") => "ruby",
        Some("ts") | Some("tsx") | Some("mts") | Some("cts") => "typescript",
        Some("py") | Some("pyi") => "python",
        Some("go") => "go",
        _ => "unknown",
    }
}

/// Build the metric points for one run. `findings` drives the per-combination
/// `stratify.findings` counts and the cycle/boundary/duplication gauges; the
/// remaining scalars come from scan aggregates passed by the caller.
pub fn report_to_metrics(
    report: &Report,
    files_scanned: u64,
    functions: u64,
    complexity_max: u32,
    complexity_mean: f64,
    duration_ms: u64,
) -> Vec<MetricPoint> {
    use std::collections::BTreeMap;

    let mut grouped: BTreeMap<(String, &'static str, &'static str, &'static str), u64> =
        BTreeMap::new();
    let mut by_rule: BTreeMap<&str, u64> = BTreeMap::new();
    for f in &report.findings {
        *by_rule.entry(f.rule.as_str()).or_insert(0) += 1;
        let key = (
            f.rule.clone(),
            severity_str(f.severity),
            lang_of(&f.span.file),
            confidence_str(f.confidence),
        );
        *grouped.entry(key).or_insert(0) += 1;
    }

    let mut out = Vec::new();
    for ((rule, sev, lang, conf), count) in grouped {
        out.push(MetricPoint {
            name: "stratify.findings".into(),
            value: count as f64,
            attributes: vec![
                ("rule".into(), rule),
                ("severity".into(), sev.into()),
                ("language".into(), lang.into()),
                ("confidence".into(), conf.into()),
            ],
        });
    }

    let scalar = |name: &str, value: f64| MetricPoint {
        name: name.into(),
        value,
        attributes: vec![],
    };
    out.push(scalar("stratify.cycles", *by_rule.get("cycle").unwrap_or(&0) as f64));
    out.push(scalar(
        "stratify.boundary_violations",
        *by_rule.get("boundary").unwrap_or(&0) as f64,
    ));
    out.push(scalar(
        "stratify.duplication.regions",
        *by_rule.get("duplication").unwrap_or(&0) as f64,
    ));
    out.push(scalar("stratify.complexity.max", complexity_max as f64));
    out.push(scalar("stratify.complexity.mean", complexity_mean));
    out.push(scalar("stratify.files_scanned", files_scanned as f64));
    out.push(scalar("stratify.functions", functions as f64));
    out.push(scalar("stratify.scan.duration_ms", duration_ms as f64));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use stratify_core::{Finding, ir::Span};

    fn finding(rule: &str, sev: Severity, file: &str, conf: Confidence) -> Finding {
        Finding {
            rule: rule.into(),
            severity: sev,
            message: "m".into(),
            span: Span {
                file: file.into(),
                start_byte: 0,
                end_byte: 1,
                start_line: 1,
            },
            confidence: conf,
        }
    }

    #[test]
    fn lang_of_maps_extensions() {
        assert_eq!(lang_of("a/b.go"), "go");
        assert_eq!(lang_of("x.tsx"), "typescript");
        assert_eq!(lang_of("no_ext"), "unknown");
    }

    #[test]
    fn findings_grouped_and_counted() {
        let report = Report::new(vec![
            finding("dead_code", Severity::Warning, "a.go", Confidence::Certain),
            finding("dead_code", Severity::Warning, "b.go", Confidence::Certain),
            finding("cycle", Severity::Warning, "c.rb", Confidence::Certain),
        ]);
        let m = report_to_metrics(&report, 3, 5, 9, 4.5, 42);

        let dc = m
            .iter()
            .find(|p| p.name == "stratify.findings"
                && p.attributes.contains(&("rule".into(), "dead_code".into())))
            .unwrap();
        assert_eq!(dc.value, 2.0);
        assert!(dc.attributes.contains(&("language".into(), "go".into())));

        let cycles = m.iter().find(|p| p.name == "stratify.cycles").unwrap();
        assert_eq!(cycles.value, 1.0);
        let cmax = m.iter().find(|p| p.name == "stratify.complexity.max").unwrap();
        assert_eq!(cmax.value, 9.0);
        let cmean = m.iter().find(|p| p.name == "stratify.complexity.mean").unwrap();
        assert_eq!(cmean.value, 4.5);
        let dur = m.iter().find(|p| p.name == "stratify.scan.duration_ms").unwrap();
        assert_eq!(dur.value, 42.0);
    }
}
