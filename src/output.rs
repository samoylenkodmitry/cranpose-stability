use crate::Report;
use serde_json::{Value, json};

const RULES: [(&str, &str); 6] = [
    ("CP000", "Rust syntax could not be analyzed"),
    ("CP001", "Callback parameter always changes"),
    ("CP002", "Opaque parameter disables skipping"),
    ("CP003", "Shared mutation may be missed"),
    ("CP004", "Parameter cannot use the comparison slot"),
    ("CP005", "Parameter stability is unknown"),
];

/// Emit SARIF 2.1.0 with source locations for CI/code-scanning systems.
pub fn sarif(report: &Report) -> Value {
    json!({
        "$schema":"https://json.schemastore.org/sarif-2.1.0.json","version":"2.1.0",
        "runs":[{"tool":{"driver":{"name":"cranpose-stability","version":env!("CARGO_PKG_VERSION"),"informationUri":"https://github.com/samoylenkodmitry/cranpose-stability",
            "rules":RULES.iter().map(|(id,title)|json!({"id":id,"shortDescription":{"text":title},"helpUri":format!("https://github.com/samoylenkodmitry/cranpose-stability#{}",id.to_lowercase())})).collect::<Vec<_>>() }},
            "columnKind":"utf16CodeUnits",
            "results":report.diagnostics.iter().map(|d|json!({
                "ruleId":d.rule,"level":d.severity,"message":{"text":format!("{}\n{}",d.message,d.help)},
                "locations":[{"physicalLocation":{"artifactLocation":{"uri":uri(&d.path),"uriBaseId":"%SRCROOT%"},"region":{"startLine":d.location.line,"startColumn":d.location.column,"endLine":d.location.end_line,"endColumn":d.location.end_column}}}],
                "partialFingerprints":{"cranposeParameter/v1":format!("{}|{}|{}|{}",d.path,d.function.as_deref().unwrap_or(""),d.parameter.as_deref().unwrap_or(""),d.rule)}
            })).collect::<Vec<_>>()
        }]
    })
}
fn uri(path: &str) -> String {
    path.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_./~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}
fn escape(s: &str, property: bool) -> String {
    let s = s
        .replace('%', "%25")
        .replace('\r', "%0D")
        .replace('\n', "%0A");
    if property {
        s.replace(',', "%2C").replace(':', "%3A")
    } else {
        s
    }
}
/// Emit escaped GitHub workflow annotations.
pub fn github(report: &Report) -> String {
    report
        .diagnostics
        .iter()
        .map(|d| {
            format!(
                "::{} file={},line={},col={},endLine={},endColumn={},title={}::{}\n",
                if d.severity == "note" {
                    "notice"
                } else {
                    &d.severity
                },
                escape(&d.path, true),
                d.location.line,
                d.location.column,
                d.location.end_line,
                d.location.end_column,
                escape(&format!("Cranpose {}", d.rule), true),
                escape(&format!("{} {}", d.message, d.help), false)
            )
        })
        .collect()
}
/// A compact compiler-style human-readable report.
pub fn text(report: &Report) -> String {
    let mut out = String::new();
    for d in &report.diagnostics {
        out.push_str(&format!(
            "{}:{}:{}: {}[{}]: {}\n  help: {}\n",
            d.path, d.location.line, d.location.column, d.severity, d.rule, d.message, d.help
        ));
    }
    out.push_str(&format!(
        "{} composables in {} files; {} findings.\n",
        report.composables.len(),
        report.files_analyzed,
        report.diagnostics.len()
    ));
    out
}
