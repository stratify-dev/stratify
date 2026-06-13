use serde::Serialize;
use stratify_core::{Report, Severity};

/// Render a report as a SARIF 2.1.0 document. GitHub code scanning and GitLab
/// render this as inline annotations.
pub fn render(report: &Report) -> String {
    serde_json::to_string_pretty(&build(report)).expect("sarif serializes")
}

fn level_of(sev: Severity) -> &'static str {
    match sev {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Info => "note",
    }
}

fn rule_description(rule: &str) -> &'static str {
    match rule {
        "dead_code" => "Unused code: functions never reached from an entrypoint.",
        "duplication" => "Duplicated code blocks.",
        "complexity" => "Functions with high cyclomatic complexity.",
        "hotspot" => "Complex code that also changes frequently.",
        "cycle" => "Circular dependencies between files.",
        "boundary" => "Imports that violate configured layer boundaries.",
        _ => "Stratify finding.",
    }
}

fn build(report: &Report) -> Sarif {
    // Distinct rule ids, in first-seen order, become the driver's rule metadata.
    let mut seen: Vec<String> = Vec::new();
    for f in &report.findings {
        if !seen.contains(&f.rule) {
            seen.push(f.rule.clone());
        }
    }
    let rules = seen
        .iter()
        .map(|id| RuleMeta {
            id: id.clone(),
            name: id.clone(),
            short_description: Text {
                text: rule_description(id).to_string(),
            },
        })
        .collect();

    let results = report
        .findings
        .iter()
        .map(|f| SarifResult {
            rule_id: f.rule.clone(),
            level: level_of(f.severity),
            message: Text {
                text: f.message.clone(),
            },
            locations: vec![Location {
                physical_location: PhysicalLocation {
                    artifact_location: ArtifactLocation {
                        uri: f.span.file.clone(),
                    },
                    region: Region {
                        start_line: f.span.start_line.max(1),
                    },
                },
            }],
        })
        .collect();

    Sarif {
        schema: "https://json.schemastore.org/sarif-2.1.0.json",
        version: "2.1.0",
        runs: vec![Run {
            tool: Tool {
                driver: Driver {
                    name: "Stratify",
                    information_uri: "https://github.com/stratify-dev/stratify",
                    version: env!("CARGO_PKG_VERSION"),
                    rules,
                },
            },
            results,
        }],
    }
}

#[derive(Serialize)]
struct Sarif {
    #[serde(rename = "$schema")]
    schema: &'static str,
    version: &'static str,
    runs: Vec<Run>,
}

#[derive(Serialize)]
struct Run {
    tool: Tool,
    results: Vec<SarifResult>,
}

#[derive(Serialize)]
struct Tool {
    driver: Driver,
}

#[derive(Serialize)]
struct Driver {
    name: &'static str,
    #[serde(rename = "informationUri")]
    information_uri: &'static str,
    version: &'static str,
    rules: Vec<RuleMeta>,
}

#[derive(Serialize)]
struct RuleMeta {
    id: String,
    name: String,
    #[serde(rename = "shortDescription")]
    short_description: Text,
}

#[derive(Serialize)]
struct SarifResult {
    #[serde(rename = "ruleId")]
    rule_id: String,
    level: &'static str,
    message: Text,
    locations: Vec<Location>,
}

#[derive(Serialize)]
struct Location {
    #[serde(rename = "physicalLocation")]
    physical_location: PhysicalLocation,
}

#[derive(Serialize)]
struct PhysicalLocation {
    #[serde(rename = "artifactLocation")]
    artifact_location: ArtifactLocation,
    region: Region,
}

#[derive(Serialize)]
struct ArtifactLocation {
    uri: String,
}

#[derive(Serialize)]
struct Region {
    #[serde(rename = "startLine")]
    start_line: usize,
}

#[derive(Serialize)]
struct Text {
    text: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use stratify_core::ir::Span;
    use stratify_core::{Confidence, Finding, Severity};

    fn finding(rule: &str, sev: Severity, file: &str, line: usize) -> Finding {
        Finding {
            rule: rule.into(),
            severity: sev,
            message: format!("{rule} message"),
            span: Span {
                file: file.into(),
                start_byte: 0,
                end_byte: 1,
                start_line: line,
            },
            confidence: Confidence::Certain,
        }
    }

    #[test]
    fn renders_valid_sarif_shape() {
        let report = Report::new(vec![
            finding("dead_code", Severity::Warning, "A.java", 5),
            finding("complexity", Severity::Info, "b.rb", 1),
        ]);
        let v: serde_json::Value = serde_json::from_str(&render(&report)).unwrap();
        assert_eq!(v["version"], "2.1.0");
        assert_eq!(v["runs"][0]["tool"]["driver"]["name"], "Stratify");
        // two distinct rules
        assert_eq!(
            v["runs"][0]["tool"]["driver"]["rules"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        // two results
        let results = v["runs"][0]["results"].as_array().unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["ruleId"], "dead_code");
        assert_eq!(results[0]["level"], "warning");
        assert_eq!(
            results[0]["locations"][0]["physicalLocation"]["artifactLocation"]["uri"],
            "A.java"
        );
        assert_eq!(
            results[0]["locations"][0]["physicalLocation"]["region"]["startLine"],
            5
        );
        // Info maps to SARIF "note"
        assert_eq!(results[1]["level"], "note");
    }

    #[test]
    fn empty_report_is_valid_sarif() {
        let v: serde_json::Value = serde_json::from_str(&render(&Report::new(vec![]))).unwrap();
        assert_eq!(v["version"], "2.1.0");
        assert!(v["runs"][0]["results"].as_array().unwrap().is_empty());
        assert!(v["runs"][0]["tool"]["driver"]["rules"]
            .as_array()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn distinct_rules_dedupe_in_driver() {
        let report = Report::new(vec![
            finding("dead_code", Severity::Warning, "a", 1),
            finding("dead_code", Severity::Warning, "b", 2),
        ]);
        let v: serde_json::Value = serde_json::from_str(&render(&report)).unwrap();
        assert_eq!(
            v["runs"][0]["tool"]["driver"]["rules"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(v["runs"][0]["results"].as_array().unwrap().len(), 2);
    }
}
