//! `oxim profile`: checks device profiles and runs their fixtures.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::Subcommand;
use oxim_devices::{Profile, TestOptions, test_profile};

use crate::CliResult;
use crate::components;
use crate::settings::Settings;

/// Device profile commands.
#[derive(Debug, Subcommand)]
pub(crate) enum ProfileCommand {
    /// Check a profile file and the files it references.
    Validate {
        /// The profile (profile.yaml).
        file: PathBuf,
    },
    /// Replay a profile's fixtures through its channel and compare the
    /// outcome with the expectations.
    Test {
        /// The profile (profile.yaml).
        file: PathBuf,
        /// Seconds each fixture may take.
        #[arg(long, default_value_t = 30)]
        timeout: u64,
        /// Write the actual normalized content to the fixtures' expected
        /// files instead of comparing (for profile authors).
        #[arg(long)]
        bless: bool,
    },
}

/// Runs a profile command.
pub(crate) fn run(command: ProfileCommand, out: &mut impl Write) -> CliResult<()> {
    match command {
        ProfileCommand::Validate { file } => {
            let loaded = Profile::load(&file)?;
            let profile = &loaded.profile;
            writeln!(
                out,
                "{}: {} {} ({}), {} fixture(s), verification level {}",
                file.display(),
                profile.vendor,
                profile.model,
                profile.id,
                profile.fixtures.len(),
                profile.verification.level
            )?;
            Ok(())
        }
        ProfileCommand::Test {
            file,
            timeout,
            bless,
        } => {
            let loaded = Profile::load(&file)?;
            let data = std::env::temp_dir().join(format!(
                "oxim-profile-test-{}-{}",
                loaded.profile.id,
                std::process::id()
            ));
            std::fs::create_dir_all(&data)?;
            let options = TestOptions {
                fixture_timeout: Duration::from_secs(timeout),
                bless,
                ..TestOptions::default()
            };
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            let report = runtime.block_on(test_profile(
                &loaded,
                || components::registry(&test_settings(&loaded.dir, &data)),
                &options,
            ));
            runtime.shutdown_timeout(Duration::from_secs(2));
            let _ = std::fs::remove_dir_all(&data);
            writeln!(out, "{report}")?;
            if report.passed() {
                Ok(())
            } else {
                Err("the profile did not pass its fixtures".into())
            }
        }
    }
}

/// Settings for a profile test: code tables resolve against the profile's
/// directory and databases live in a scratch directory.
fn test_settings(profile_dir: &Path, data_dir: &Path) -> Settings {
    Settings {
        tables_dir: profile_dir.to_owned(),
        data_dir: data_dir.to_owned(),
        ..Settings::default()
    }
}
