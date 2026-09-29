//! `oxim shadow`: replays captured Mirth Connect traffic through an OXIM
//! channel and compares the outputs (see `oxim-shadow`).

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use clap::Args;
use oxim_core::ChannelConfig;
use oxim_shadow::{
    CompareOptions, DEFAULT_IGNORE, DestinationCapture, Framing, ShadowOptions, Traffic, is_pcap,
    shadow,
};

use crate::CliResult;
use crate::components;
use crate::settings::Settings;

/// Options of `oxim shadow`.
#[derive(Debug, Args)]
pub(crate) struct ShadowArgs {
    /// The OXIM channel file to check.
    #[arg(long)]
    channel: PathBuf,
    /// A PCAP/PCAPNG capture, or an .oximcap capture of the inbound side.
    #[arg(long)]
    capture: PathBuf,
    /// The port the Mirth channel listens on (PCAP captures).
    #[arg(long, default_value_t = 0)]
    inbound_port: u16,
    /// Framing of the inbound side: mllp, astm or astm-raw.
    #[arg(long, default_value = "mllp")]
    framing: String,
    /// A destination and where Mirth sent to it: `id=port`, `id=port:astm`
    /// or, with .oximcap captures, `id=file.oximcap` (repeatable).
    #[arg(long = "destination", required = true)]
    destinations: Vec<String>,
    /// A path ignored in the comparison (repeatable); MSH-7 and MSH-10 by
    /// default.
    #[arg(long = "ignore")]
    ignore: Vec<String>,
    /// Pair messages by this path instead of by order.
    #[arg(long)]
    key: Option<String>,
    /// Show differing values in the report (they may be patient data).
    #[arg(long)]
    show_values: bool,
    /// Write the report here: Markdown, or JSON for a .json name.
    #[arg(long)]
    report: Option<PathBuf>,
    /// Seconds one message may take in the replay.
    #[arg(long, default_value_t = 10)]
    timeout: u64,
}

fn framing(text: &str) -> CliResult<Framing> {
    match text {
        "mllp" => Ok(Framing::Mllp),
        "astm" => Ok(Framing::Astm),
        "astm-raw" => Ok(Framing::AstmRaw),
        other => Err(format!("unknown framing {other:?}; use mllp, astm or astm-raw").into()),
    }
}

/// Runs a shadow comparison. Returns whether OXIM's output matched.
pub(crate) fn run(settings: &Settings, args: ShadowArgs, out: &mut impl Write) -> CliResult<bool> {
    let text = std::fs::read_to_string(&args.channel)
        .map_err(|e| format!("cannot read {}: {e}", args.channel.display()))?;
    let channel = ChannelConfig::from_yaml(&text)?;
    let capture = std::fs::read(&args.capture)
        .map_err(|e| format!("cannot read {}: {e}", args.capture.display()))?;
    let pcap = is_pcap(&capture);

    let mut destinations = Vec::new();
    let mut captures = Vec::new();
    for spec in &args.destinations {
        let (id, target) = spec
            .split_once('=')
            .ok_or_else(|| format!("--destination {spec:?}: use id=port or id=file.oximcap"))?;
        let (target, destination_framing) = match target.rsplit_once(':') {
            Some((target, kind)) if kind.chars().all(|c| c.is_ascii_alphabetic() || c == '-') => {
                (target, framing(kind)?)
            }
            _ => (target, Framing::Mllp),
        };
        let port = if pcap {
            target
                .parse()
                .map_err(|_| format!("--destination {spec:?}: the port must be a number"))?
        } else {
            let file = oxim_capture::Capture::load(target)
                .map_err(|e| format!("cannot read {target}: {e}"))?;
            captures.push((id.to_owned(), file));
            0
        };
        destinations.push(DestinationCapture {
            destination: id.to_owned(),
            port,
            framing: destination_framing,
        });
    }
    if pcap && args.inbound_port == 0 {
        return Err("--inbound-port is required for PCAP captures".into());
    }
    let ignore = if args.ignore.is_empty() {
        DEFAULT_IGNORE.iter().map(|p| (*p).to_owned()).collect()
    } else {
        args.ignore.clone()
    };
    let options = ShadowOptions {
        inbound_port: args.inbound_port,
        inbound_framing: framing(&args.framing)?,
        destinations,
        compare: CompareOptions {
            ignore,
            key: args.key.clone(),
            show_values: args.show_values,
        },
        timeout: Duration::from_secs(args.timeout.max(1)),
    };
    let traffic = if pcap {
        Traffic::from_pcap(&capture, &options)?
    } else {
        let inbound = oxim_capture::Capture::from_slice(&capture).map_err(|e| {
            format!(
                "{} is neither PCAP nor .oximcap: {e}",
                args.capture.display()
            )
        })?;
        Traffic::from_captures(&inbound, &captures, &options)
    };

    // Steps that keep state (order cache, device registry) write to a
    // scratch directory, never to the running installation's databases.
    let scratch = std::env::temp_dir().join(format!("oxim-shadow-{}", std::process::id()));
    std::fs::create_dir_all(&scratch)?;
    let mut replay_settings = settings.clone();
    replay_settings.data_dir = scratch.clone();
    let registry = components::registry(&replay_settings);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let report = runtime.block_on(shadow(&channel, registry, &traffic, &options));
    runtime.shutdown_timeout(Duration::from_secs(2));
    let _ = std::fs::remove_dir_all(&scratch);
    let report = report?;

    let markdown = report.to_markdown();
    match &args.report {
        Some(path) => {
            let body = if path.extension().is_some_and(|e| e == "json") {
                report.to_json()?
            } else {
                markdown.clone()
            };
            std::fs::write(path, body)
                .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
            writeln!(out, "report written to {}", path.display())?;
        }
        None => write!(out, "{markdown}")?,
    }
    Ok(report.is_identical())
}
