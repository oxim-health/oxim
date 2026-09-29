//! `oxim backup`, `oxim restore` and scheduled backups.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use oxim_model::Timestamp;
use oxim_server::backup::{self, BackupSources};
use tracing::{error, info};

use crate::CliResult;
use crate::settings::Settings;

/// What backups of this installation contain.
pub(crate) fn sources(settings: &Settings) -> BackupSources {
    BackupSources {
        data_dir: settings.data_dir.clone(),
        channels_dir: settings.channels_dir.clone(),
        tables_dir: settings.tables_dir.clone(),
        scripts_dir: Some(settings.scripts_dir.clone()),
        config_file: settings.config_file.clone(),
    }
}

fn now() -> Timestamp {
    Timestamp::from_system_time(SystemTime::now()).unwrap_or(Timestamp::from_unix_nanos(0))
}

fn megabytes(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1_000_000.0)
}

/// `oxim backup [--out <file>]`: a backup into `out`, or into the backup
/// directory (keeping the newest `backups.keep`).
pub(crate) fn backup(
    settings: &Settings,
    out: Option<PathBuf>,
    w: &mut impl Write,
) -> CliResult<()> {
    let now = now();
    let (path, prune) = match out {
        Some(path) => (path, false),
        None => (settings.backups_dir().join(backup::backup_name(now)), true),
    };
    let manifest = backup::create(&sources(settings), &path, now)?;
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    writeln!(
        w,
        "backup written to {} ({} files, {})",
        path.display(),
        manifest.files.len(),
        megabytes(size)
    )?;
    if prune {
        let deleted = backup::prune(&settings.backups_dir(), settings.backups.keep.max(1))?;
        if deleted > 0 {
            writeln!(w, "deleted {deleted} older backup(s)")?;
        }
    }
    Ok(())
}

/// `oxim restore <archive> [--force]`.
pub(crate) fn restore(
    settings: &Settings,
    archive: &Path,
    force: bool,
    w: &mut impl Write,
) -> CliResult<()> {
    let report = backup::restore(archive, &sources(settings), force, now())?;
    writeln!(
        w,
        "restored {} files from a backup of {} (OXIM {})",
        report.manifest.files.len(),
        report.manifest.created_at,
        report.manifest.oxim_version
    )?;
    if let Some(previous) = report.previous {
        writeln!(w, "the replaced files were moved to {}", previous.display())?;
    }
    writeln!(w, "start OXIM again to use the restored data")?;
    Ok(())
}

/// The minutes after midnight of an `HH:MM` schedule.
pub(crate) fn parse_schedule(text: &str) -> Result<u32, String> {
    let invalid = || format!("backups.schedule must be HH:MM, not {text:?}");
    let (hours, minutes) = text.trim().split_once(':').ok_or_else(invalid)?;
    let hours: u32 = hours.parse().map_err(|_| invalid())?;
    let minutes: u32 = minutes.parse().map_err(|_| invalid())?;
    if hours > 23 || minutes > 59 {
        return Err(invalid());
    }
    Ok(hours * 60 + minutes)
}

/// The next time of day `minute_of_day` (at `utc_offset` minutes) after
/// `now`.
fn next_run(now: Timestamp, minute_of_day: u32, utc_offset: i16) -> Timestamp {
    const DAY: i64 = 86_400;
    let local = now.unix_nanos() / 1_000_000_000 + i64::from(utc_offset) * 60;
    let midnight = local.div_euclid(DAY) * DAY;
    let mut next = midnight + i64::from(minute_of_day) * 60;
    if next <= local {
        next += DAY;
    }
    Timestamp::from_unix_nanos((next - i64::from(utc_offset) * 60) * 1_000_000_000)
}

/// Takes a backup every day at `backups.schedule` while OXIM runs.
pub(crate) async fn schedule_loop(settings: Settings) {
    let Some(schedule) = settings.backups.schedule.clone() else {
        return;
    };
    let minute_of_day = match parse_schedule(&schedule) {
        Ok(minute) => minute,
        Err(e) => {
            error!(error = %e, "scheduled backups are off");
            return;
        }
    };
    loop {
        let now = now();
        let next = next_run(now, minute_of_day, settings.backups.utc_offset);
        let wait = u64::try_from(next.unix_nanos() - now.unix_nanos()).unwrap_or(0);
        tokio::time::sleep(Duration::from_nanos(wait)).await;
        let settings = settings.clone();
        let result = tokio::task::spawn_blocking(move || {
            let mut out = Vec::new();
            backup(&settings, None, &mut out).map(|()| String::from_utf8_lossy(&out).into_owned())
        })
        .await;
        match result {
            Ok(Ok(summary)) => info!(summary = summary.trim(), "scheduled backup taken"),
            Ok(Err(e)) => error!(error = %e, "scheduled backup failed"),
            Err(e) => error!(error = %e, "scheduled backup failed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: i64) -> Timestamp {
        Timestamp::from_unix_nanos(seconds * 1_000_000_000)
    }

    #[test]
    fn schedules_the_next_daily_run() {
        assert_eq!(parse_schedule("02:30").unwrap(), 150);
        for bad in ["24:00", "2", "02:60", "ab:cd"] {
            assert!(parse_schedule(bad).is_err(), "{bad}");
        }
        // 1970-01-01T01:00:00Z: today's 02:30 UTC is still ahead.
        assert_eq!(next_run(at(3600), 150, 0), at(9000));
        // At 03:00 UTC the next run is tomorrow.
        assert_eq!(next_run(at(10_800), 150, 0), at(86_400 + 9000));
        // 02:30 at UTC+3 is 23:30 UTC the day before.
        assert_eq!(next_run(at(3600), 150, 180), at(86_400 - 1800));
    }

    #[test]
    fn backs_up_and_restores_through_the_cli() {
        let dir = tempfile::tempdir().unwrap();
        let settings = Settings {
            data_dir: dir.path().join("data"),
            channels_dir: dir.path().join("channels"),
            tables_dir: dir.path().join("tables"),
            scripts_dir: dir.path().join("scripts"),
            ..Settings::default()
        };
        for path in [
            &settings.data_dir,
            &settings.channels_dir,
            &settings.tables_dir,
        ] {
            std::fs::create_dir_all(path).unwrap();
        }
        std::fs::write(settings.channels_dir.join("lab.yaml"), "id: lab\n").unwrap();
        let mut out = Vec::new();
        backup(&settings, None, &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("backup written to "), "{text}");
        let archive = backup::list(&settings.backups_dir()).remove(0);
        std::fs::remove_file(settings.channels_dir.join("lab.yaml")).unwrap();
        let mut out = Vec::new();
        restore(
            &settings,
            &settings.backups_dir().join(&archive.name),
            false,
            &mut out,
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(settings.channels_dir.join("lab.yaml")).unwrap(),
            "id: lab\n"
        );
    }
}
