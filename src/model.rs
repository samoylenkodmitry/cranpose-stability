use serde::{Deserialize, Serialize};

/// One UTF-8 Rust source file. Module names exclude the crate name.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceFile {
    pub path: String,
    pub source: String,
    #[serde(default = "default_crate")]
    pub crate_name: String,
    #[serde(default)]
    pub module: Vec<String>,
    #[serde(default)]
    pub crate_aliases: std::collections::BTreeMap<String, String>,
}
fn default_crate() -> String {
    "crate".into()
}

/// An explicit, reviewable suppression. It hides the diagnostic, never the badge.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Allow {
    pub path: String,
    pub function: String,
    pub parameter: String,
    pub rule: String,
    pub reason: String,
}

/// Shared editor and CI configuration.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub stable_types: Vec<String>,
    pub exclude: Vec<String>,
    pub allow: Vec<Allow>,
}

/// A source location with byte and UTF-16 offsets; all ranges are end-exclusive.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Location {
    pub start: usize,
    pub end: usize,
    pub utf16_start: usize,
    pub utf16_end: usize,
    pub line: usize,
    pub column: usize,
    pub end_line: usize,
    pub end_column: usize,
}
/// Byte and UTF-16 offsets of line starts, so each location costs one line scan.
pub(crate) struct Lines {
    bytes: Vec<usize>,
    utf16: Vec<usize>,
}
impl Lines {
    pub(crate) fn new(source: &str) -> Self {
        let (mut bytes, mut utf16, mut units) = (vec![0], vec![0], 0);
        for (i, c) in source.char_indices() {
            units += c.len_utf16();
            if c == '\n' {
                bytes.push(i + 1);
                utf16.push(units);
            }
        }
        Self { bytes, utf16 }
    }
}
impl Location {
    pub(crate) fn span(source: &str, span: proc_macro2::Span) -> Self {
        Self::within(source, &Lines::new(source), span)
    }
    pub(crate) fn within(source: &str, lines: &Lines, span: proc_macro2::Span) -> Self {
        // (byte offset, UTF-16 offset, one-based UTF-16 column)
        let at = |p: proc_macro2::LineColumn| {
            let line = p.line.saturating_sub(1).min(lines.bytes.len() - 1);
            let base = lines.bytes[line];
            let (mut bytes, mut units) = (0, 0);
            for c in source
                .get(base..)
                .unwrap_or_default()
                .chars()
                .take(p.column)
            {
                bytes += c.len_utf8();
                units += c.len_utf16();
            }
            (
                (base + bytes).min(source.len()),
                lines.utf16[line] + units,
                units + 1,
            )
        };
        let (start, utf16_start, column) = at(span.start());
        let (end, utf16_end, end_column) = at(span.end());
        Self {
            start,
            end,
            utf16_start,
            utf16_end,
            line: span.start().line,
            column,
            end_line: span.end().line,
            end_column,
        }
    }
}

/// What can be established from source, independently of measured runtime cost.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Stability {
    Stable,
    Unstable,
    Unknown,
    Incompatible,
}

/// The actual mechanism, distinguished from the presentation badge.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Effect {
    ValueComparison,
    AlwaysChanged,
    SkippingDisabled,
    SharedMutation,
    Unproven,
    InvalidParameter,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Parameter {
    pub name: String,
    pub type_text: String,
    pub location: Location,
    pub stability: Stability,
    pub effect: Effect,
    pub reason: String,
    pub advice: String,
    pub rule: Option<String>,
    pub suppressed: Option<String>,
    /// Canonical path of the type that decided the verdict, such as `cranpose_ui::modifier::Modifier`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_type: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Composable {
    pub path: String,
    pub name: String,
    pub qualified_name: String,
    pub location: Location,
    pub skip_mode: String,
    pub parameters: Vec<Parameter>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostic {
    pub path: String,
    pub rule: String,
    pub severity: String,
    pub message: String,
    pub help: String,
    pub location: Location,
    pub function: Option<String>,
    pub parameter: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub schema_version: u32,
    pub policy: String,
    pub files_analyzed: usize,
    pub composables: Vec<Composable>,
    pub diagnostics: Vec<Diagnostic>,
}
impl Report {
    /// Unknown contracts are informational unless the caller elects to deny them.
    pub fn has_findings(&self, deny_unknown: bool) -> bool {
        self.diagnostics
            .iter()
            .any(|d| d.severity != "note" || deny_unknown)
    }
}
