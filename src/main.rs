use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use cranpose_stability::{
    Report, output,
    project::{ProjectRequest, analyze_project},
};
use std::{
    io::{Read, Write},
    path::PathBuf,
};

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    Text,
    Json,
    Sarif,
    Github,
}
#[derive(Parser)]
#[command(
    version,
    about = "Inspect Cranpose composable parameters without changing application builds"
)]
struct Args {
    #[arg(long, default_value = ".")]
    root: PathBuf,
    #[arg(long, value_enum, default_value = "text")]
    format: Format,
    /// Treat unknown contracts as failing findings as well.
    #[arg(long)]
    deny_unknown: bool,
    /// Read a ProjectRequest JSON object on stdin, including unsaved file overlays.
    #[arg(long)]
    stdin_json: bool,
    /// Write the report to this file instead of stdout.
    #[arg(long)]
    output: Option<PathBuf>,
}
fn run() -> Result<i32> {
    let args = Args::parse();
    let request = if args.stdin_json {
        let mut input = String::new();
        std::io::stdin()
            .take(16 * 1024 * 1024 + 1)
            .read_to_string(&mut input)?;
        if input.len() > 16 * 1024 * 1024 {
            bail!("request exceeds 16 MiB");
        }
        serde_json::from_str(&input).context("invalid ProjectRequest JSON")?
    } else {
        ProjectRequest {
            root: args.root,
            overlays: Vec::new(),
            only: Vec::new(),
        }
    };
    let report = analyze_project(&request)?;
    let encoded = render(&report, args.format)?;
    if let Some(path) = args.output {
        std::fs::write(path, encoded)?;
    } else {
        std::io::stdout().write_all(encoded.as_bytes())?;
    }
    // Syntax errors distinguish incomplete analysis from lint findings.
    Ok(if report.diagnostics.iter().any(|d| d.rule == "CP000") {
        2
    } else if report.has_findings(args.deny_unknown) {
        1
    } else {
        0
    })
}
fn render(report: &Report, format: Format) -> Result<String> {
    Ok(match format {
        Format::Text => output::text(report),
        Format::Github => output::github(report),
        Format::Json => serde_json::to_string_pretty(report)? + "\n",
        Format::Sarif => serde_json::to_string_pretty(&output::sarif(report))? + "\n",
    })
}
fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("cranpose-stability: {e:#}");
            std::process::exit(2);
        }
    }
}
