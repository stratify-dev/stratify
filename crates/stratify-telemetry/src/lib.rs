//! Pure mapping from a Stratify Report + scan aggregates to OTLP-ready
//! metric points and a per-run event. No network, no OTel SDK here.

pub mod emit;
pub use emit::{emit, TelemetryConfig};

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

use std::collections::BTreeSet;

/// Typed attribute value for the per-run event.
#[derive(Debug, Clone, PartialEq)]
pub enum AttrValue {
    Str(String),
    Int(i64),
}

/// Git metadata for the run. Fields are None outside a git repo.
#[derive(Debug, Clone, Default)]
pub struct GitMeta {
    pub commit: Option<String>,
    pub branch: Option<String>,
    pub remote_url: Option<String>,
}

/// One structured log record summarizing a run. Holds the high-cardinality
/// fields (commit, branch) that must never go on metric attributes.
#[derive(Debug, Clone, PartialEq)]
pub struct RunEvent {
    pub body: String,
    pub attributes: Vec<(String, AttrValue)>,
}

/// Build the per-run event from the report, git metadata, project name,
/// duration, and the set of languages seen.
pub fn report_to_event(
    report: &Report,
    git: &GitMeta,
    project: &str,
    duration_ms: u64,
    languages: &BTreeSet<String>,
) -> RunEvent {
    let mut attrs: Vec<(String, AttrValue)> = Vec::new();
    attrs.push(("project".into(), AttrValue::Str(project.into())));
    if let Some(c) = &git.commit {
        attrs.push(("commit".into(), AttrValue::Str(c.clone())));
    }
    if let Some(b) = &git.branch {
        attrs.push(("branch".into(), AttrValue::Str(b.clone())));
    }
    attrs.push((
        "total_findings".into(),
        AttrValue::Int(report.findings.len() as i64),
    ));

    let count_sev = |s: Severity| {
        report.findings.iter().filter(|f| f.severity == s).count() as i64
    };
    attrs.push(("info".into(), AttrValue::Int(count_sev(Severity::Info))));
    attrs.push(("warning".into(), AttrValue::Int(count_sev(Severity::Warning))));
    attrs.push(("error".into(), AttrValue::Int(count_sev(Severity::Error))));

    let mut by_rule: std::collections::BTreeMap<&str, i64> = std::collections::BTreeMap::new();
    for f in &report.findings {
        *by_rule.entry(f.rule.as_str()).or_insert(0) += 1;
    }
    for (rule, n) in by_rule {
        attrs.push((format!("rule.{rule}"), AttrValue::Int(n)));
    }

    attrs.push(("duration_ms".into(), AttrValue::Int(duration_ms as i64)));
    attrs.push((
        "languages".into(),
        AttrValue::Str(languages.iter().cloned().collect::<Vec<_>>().join(",")),
    ));

    RunEvent {
        body: "stratify.run".into(),
        attributes: attrs,
    }
}

/// Parse `OTEL_EXPORTER_OTLP_HEADERS` (`k1=v1,k2=v2`). Entries without `=` are
/// skipped. Surrounding whitespace is trimmed.
pub fn parse_headers(raw: &str) -> Vec<(String, String)> {
    raw.split(',')
        .filter_map(|pair| {
            let (k, v) = pair.split_once('=')?;
            let k = k.trim();
            if k.is_empty() {
                return None;
            }
            Some((k.to_string(), v.trim().to_string()))
        })
        .collect()
}

/// Resolve `service.name`: flag > env > git remote basename > directory name.
pub fn resolve_service_name(
    flag: Option<&str>,
    env: Option<&str>,
    git_basename: Option<&str>,
    dir_name: &str,
) -> String {
    flag.or(env)
        .or(git_basename)
        .unwrap_or(dir_name)
        .to_string()
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

    #[test]
    fn event_carries_commit_and_totals() {
        let report = Report::new(vec![
            finding("dead_code", Severity::Warning, "a.go", Confidence::Certain),
            finding("cycle", Severity::Error, "c.rb", Confidence::Likely),
        ]);
        let git = GitMeta {
            commit: Some("abc123".into()),
            branch: Some("main".into()),
            remote_url: None,
        };
        let langs = ["go".to_string(), "ruby".to_string()].into_iter().collect();
        let ev = report_to_event(&report, &git, "org/repo", 99, &langs);

        assert_eq!(ev.body, "stratify.run");
        let get = |k: &str| ev.attributes.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        assert_eq!(get("project"), Some(AttrValue::Str("org/repo".into())));
        assert_eq!(get("commit"), Some(AttrValue::Str("abc123".into())));
        assert_eq!(get("branch"), Some(AttrValue::Str("main".into())));
        assert_eq!(get("total_findings"), Some(AttrValue::Int(2)));
        assert_eq!(get("warning"), Some(AttrValue::Int(1)));
        assert_eq!(get("error"), Some(AttrValue::Int(1)));
        assert_eq!(get("info"), Some(AttrValue::Int(0)));
        assert_eq!(get("rule.dead_code"), Some(AttrValue::Int(1)));
        assert_eq!(get("rule.cycle"), Some(AttrValue::Int(1)));
        assert_eq!(get("duration_ms"), Some(AttrValue::Int(99)));
        assert_eq!(get("languages"), Some(AttrValue::Str("go,ruby".into())));
    }

    #[test]
    fn event_omits_absent_git_fields() {
        let report = Report::new(vec![]);
        let git = GitMeta { commit: None, branch: None, remote_url: None };
        let ev = report_to_event(&report, &git, "p", 0, &Default::default());
        assert!(ev.attributes.iter().all(|(n, _)| n != "commit"));
        assert!(ev.attributes.iter().all(|(n, _)| n != "branch"));
    }

    #[test]
    fn parse_headers_splits_pairs() {
        assert_eq!(
            parse_headers("a=1,b=2"),
            vec![("a".to_string(), "1".to_string()), ("b".to_string(), "2".to_string())]
        );
        assert!(parse_headers("").is_empty());
        assert_eq!(parse_headers("only=this,broken"), vec![("only".to_string(), "this".to_string())]);
    }

    #[test]
    fn service_name_precedence() {
        assert_eq!(resolve_service_name(Some("flag"), Some("env"), Some("git"), "dir"), "flag");
        assert_eq!(resolve_service_name(None, Some("env"), Some("git"), "dir"), "env");
        assert_eq!(resolve_service_name(None, None, Some("git"), "dir"), "git");
        assert_eq!(resolve_service_name(None, None, None, "dir"), "dir");
    }
}
