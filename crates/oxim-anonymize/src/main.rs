//! `oxim-anonymize`: removes protected health information from messages
//! and captures.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, ValueEnum};
use oxim_anonymize::{Anonymizer, DataKind, Options, Report, capture, detect};
use oxim_capture::Capture;

type CliResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Kind {
    /// Decide from the file extension and content.
    Auto,
    /// HL7 v2 messages.
    Hl7v2,
    /// ASTM E1394 messages.
    Astm,
    /// POCT1-A XML.
    Poct1a,
    /// JSON, by the configured paths.
    Json,
    /// XML, by the configured paths.
    Xml,
    /// An .oximcap capture.
    Capture,
}

#[derive(Debug, Parser)]
#[command(
    name = "oxim-anonymize",
    version,
    about = "Remove protected health information from HL7 v2, ASTM, POCT1-A, JSON and XML messages and .oximcap captures"
)]
struct Cli {
    /// Files to anonymize.
    #[arg(required = true)]
    inputs: Vec<PathBuf>,
    /// Output file (one input) or directory (several inputs). By default
    /// `name.anon.ext` is written next to each input.
    #[arg(long, short)]
    out: Option<PathBuf>,
    /// Overwrite existing output files.
    #[arg(long)]
    force: bool,
    /// The kind of the inputs.
    #[arg(long = "type", value_enum, default_value_t = Kind::Auto)]
    kind: Kind,
    /// Options file (YAML): free_text, specimens, json and xml rules.
    #[arg(long)]
    config: Option<PathBuf>,
    /// Pseudonym key in hexadecimal (at least 32 digits); the
    /// OXIM_ANONYMIZE_KEY environment variable works too. Runs with the same
    /// key give the same pseudonyms; without it the key is random.
    #[arg(long)]
    key: Option<String>,
    /// Days to shift every date by; random (30 to 730 days back) by default.
    #[arg(long, allow_hyphen_values = true)]
    date_shift_days: Option<i64>,
    /// Redact free text (HL7 NTE and text OBX, ASTM comments).
    #[arg(long)]
    free_text: bool,
    /// Keep specimen, order and container identifiers.
    #[arg(long)]
    keep_specimen_ids: bool,
    /// Write the report as JSON to this file.
    #[arg(long)]
    report: Option<PathBuf>,
}

fn hex(text: &str) -> CliResult<Vec<u8>> {
    let text = text.trim();
    if !text.len().is_multiple_of(2) || text.len() < 32 {
        return Err("the key must be at least 32 hexadecimal digits".into());
    }
    (0..text.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&text[i..i + 2], 16)
                .map_err(|_| "the key must be hexadecimal".into())
        })
        .collect()
}

fn is_capture(path: &Path, bytes: &[u8]) -> bool {
    path.extension().is_some_and(|ext| ext == "oximcap")
        || bytes.starts_with(br#"{"format":"oximcap""#)
}

fn output_path(input: &Path, out: Option<&Path>, several: bool) -> PathBuf {
    let name = {
        let stem = input
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        match input.extension() {
            Some(ext) => format!("{stem}.anon.{}", ext.to_string_lossy()),
            None => format!("{stem}.anon"),
        }
    };
    match out {
        Some(out) if several => {
            out.join(input.file_name().map_or(name.clone().into(), PathBuf::from))
        }
        Some(out) => out.to_owned(),
        None => input.with_file_name(name),
    }
}

fn run(cli: Cli) -> CliResult<Report> {
    let mut options = match &cli.config {
        Some(path) => Options::from_yaml(
            &std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?,
        )?,
        None => Options::default(),
    };
    options.free_text |= cli.free_text;
    if cli.keep_specimen_ids {
        options.specimens = false;
    }
    let key = cli
        .key
        .clone()
        .or_else(|| std::env::var("OXIM_ANONYMIZE_KEY").ok())
        .as_deref()
        .map(hex)
        .transpose()?;
    let anonymizer = Anonymizer::new(options, key.as_deref(), cli.date_shift_days)?;
    let mut report = anonymizer.report();
    let several = cli.inputs.len() > 1;
    if several && let Some(out) = &cli.out {
        std::fs::create_dir_all(out)?;
    }
    for input in &cli.inputs {
        let bytes = std::fs::read(input).map_err(|e| format!("{}: {e}", input.display()))?;
        let output = output_path(input, cli.out.as_deref(), several);
        if output.exists() && !cli.force {
            return Err(format!("{} exists; use --force to overwrite", output.display()).into());
        }
        let capture_input =
            cli.kind == Kind::Capture || (cli.kind == Kind::Auto && is_capture(input, &bytes));
        let result = if capture_input {
            let capture =
                Capture::from_slice(&bytes).map_err(|e| format!("{}: {e}", input.display()))?;
            capture::anonymize_capture(&anonymizer, &capture, &mut report).to_bytes()
        } else {
            let kind = match cli.kind {
                Kind::Hl7v2 => DataKind::Hl7v2,
                Kind::Astm => DataKind::Astm,
                Kind::Poct1a => DataKind::Poct1a,
                Kind::Json => DataKind::Json,
                Kind::Xml => DataKind::Xml,
                Kind::Auto | Kind::Capture => detect(&bytes).ok_or_else(|| {
                    format!(
                        "{}: cannot tell the message type; use --type",
                        input.display()
                    )
                })?,
            };
            anonymizer
                .anonymize(kind, &bytes, &mut report)
                .map_err(|e| format!("{}: {e}", input.display()))?
        };
        std::fs::write(&output, result).map_err(|e| format!("{}: {e}", output.display()))?;
        eprintln!("{} -> {}", input.display(), output.display());
    }
    if let Some(path) = &cli.report {
        std::fs::write(path, serde_json::to_vec_pretty(&report)?)?;
    }
    Ok(report)
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(report) => {
            eprint!("{report}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
