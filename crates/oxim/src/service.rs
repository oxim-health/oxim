//! Running OXIM as an operating system service: a Windows service or a
//! systemd unit.

use std::io::Write;
use std::path::Path;

use crate::CliResult;

/// The service name registered with the operating system.
pub(crate) const SERVICE_NAME: &str = "oxim";
const DISPLAY_NAME: &str = "OXIM Integration Engine";
const DESCRIPTION: &str =
    "Receives, transforms and delivers clinical messages between devices and systems.";

/// The systemd unit that runs OXIM with `config`.
pub(crate) fn systemd_unit(executable: &Path, config: &Path) -> String {
    format!(
        "[Unit]
Description={DISPLAY_NAME}
Documentation=https://github.com/oxim-health/oxim
After=network-online.target
Wants=network-online.target

[Service]
Type=exec
User=oxim
Group=oxim
ExecStart={exe} run --config {config}
Restart=on-failure
RestartSec=5
# Allow in-flight deliveries to finish before systemd kills the process.
TimeoutStopSec=60
NoNewPrivileges=true
ProtectSystem=full
PrivateTmp=true

[Install]
WantedBy=multi-user.target
",
        exe = executable.display(),
        config = config.display(),
    )
}

/// Installs the service for this executable and `config`.
pub(crate) fn install(config: &Path, out: &mut impl Write) -> CliResult<()> {
    let config = config
        .canonicalize()
        .map_err(|e| format!("cannot find {}: {e}", config.display()))?;
    platform::install(&config, out)
}

/// Removes the service.
pub(crate) fn uninstall(out: &mut impl Write) -> CliResult<()> {
    platform::uninstall(out)
}

/// Starts the installed service.
pub(crate) fn start(out: &mut impl Write) -> CliResult<()> {
    platform::start(out)
}

/// Stops the installed service.
pub(crate) fn stop(out: &mut impl Write) -> CliResult<()> {
    platform::stop(out)
}

/// Runs as the service process (called by the service manager).
pub(crate) fn run(config: &Path) -> CliResult<()> {
    platform::run(config)
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod platform {
    //! The Windows service. `define_windows_service!` generates the FFI
    //! entry point the service control manager calls.

    use std::ffi::OsString;
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, OnceLock};
    use std::time::Duration;

    use windows_service::service::{
        ServiceAccess, ServiceControl, ServiceControlAccept, ServiceErrorControl, ServiceExitCode,
        ServiceInfo, ServiceStartType, ServiceState, ServiceStatus, ServiceType,
    };
    use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
    use windows_service::{define_windows_service, service_dispatcher};

    use super::{DESCRIPTION, DISPLAY_NAME, SERVICE_NAME};
    use crate::CliResult;

    static CONFIG: OnceLock<PathBuf> = OnceLock::new();

    define_windows_service!(ffi_service_main, service_main);

    pub(super) fn run(config: &Path) -> CliResult<()> {
        let _ = CONFIG.set(config.to_path_buf());
        service_dispatcher::start(SERVICE_NAME, ffi_service_main).map_err(|e| {
            format!("cannot start as a Windows service: {e}; use `oxim run` in a console")
        })?;
        Ok(())
    }

    fn service_main(_arguments: Vec<OsString>) {
        if let Err(e) = run_service() {
            tracing::error!(error = %e, "service stopped with an error");
        }
    }

    fn status(state: ServiceState, controls: ServiceControlAccept, exit: u32) -> ServiceStatus {
        ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: controls,
            exit_code: ServiceExitCode::Win32(exit),
            checkpoint: 0,
            wait_hint: Duration::from_secs(60),
            process_id: None,
        }
    }

    fn run_service() -> CliResult<()> {
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let stop = Mutex::new(Some(stop));
        let handler = move |control| match control {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                if let Some(stop) = stop.lock().ok().and_then(|mut s| s.take()) {
                    let _ = stop.send(());
                }
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        };
        let handle = service_control_handler::register(SERVICE_NAME, handler)?;
        handle.set_service_status(status(
            ServiceState::Running,
            ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
            0,
        ))?;

        let result = (|| -> CliResult<()> {
            let config = CONFIG.get().cloned().ok_or("no configuration path")?;
            let settings = crate::settings::Settings::load(&config)?;
            let _guard = crate::logging::init(&settings.log)?;
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            runtime.block_on(crate::run::run(settings, async {
                let _ = stopped.await;
            }))
        })();

        handle.set_service_status(status(
            ServiceState::Stopped,
            ServiceControlAccept::empty(),
            u32::from(result.is_err()),
        ))?;
        result
    }

    pub(super) fn install(config: &Path, out: &mut impl Write) -> CliResult<()> {
        let manager = ServiceManager::local_computer(
            None::<&str>,
            ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
        )?;
        let info = ServiceInfo {
            name: OsString::from(SERVICE_NAME),
            display_name: OsString::from(DISPLAY_NAME),
            service_type: ServiceType::OWN_PROCESS,
            start_type: ServiceStartType::AutoStart,
            error_control: ServiceErrorControl::Normal,
            executable_path: std::env::current_exe()?,
            launch_arguments: vec![
                OsString::from("service"),
                OsString::from("run"),
                OsString::from("--config"),
                config.as_os_str().to_owned(),
            ],
            dependencies: vec![],
            account_name: None,
            account_password: None,
        };
        let service = manager.create_service(&info, ServiceAccess::CHANGE_CONFIG)?;
        service.set_description(DESCRIPTION)?;
        writeln!(
            out,
            "installed Windows service {SERVICE_NAME:?} (automatic start)"
        )?;
        writeln!(
            out,
            "start it with `oxim service start` or from services.msc"
        )?;
        Ok(())
    }

    fn open(access: ServiceAccess) -> CliResult<windows_service::service::Service> {
        let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
        Ok(manager.open_service(SERVICE_NAME, access)?)
    }

    pub(super) fn uninstall(out: &mut impl Write) -> CliResult<()> {
        let service =
            open(ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE)?;
        if service.query_status()?.current_state != ServiceState::Stopped {
            let _ = service.stop();
        }
        service.delete()?;
        writeln!(out, "removed Windows service {SERVICE_NAME:?}")?;
        Ok(())
    }

    pub(super) fn start(out: &mut impl Write) -> CliResult<()> {
        open(ServiceAccess::START)?.start::<OsString>(&[])?;
        writeln!(out, "started {SERVICE_NAME:?}")?;
        Ok(())
    }

    pub(super) fn stop(out: &mut impl Write) -> CliResult<()> {
        open(ServiceAccess::STOP)?.stop()?;
        writeln!(out, "stop requested for {SERVICE_NAME:?}")?;
        Ok(())
    }
}

#[cfg(not(windows))]
mod platform {
    //! systemd on Linux. Other Unix systems run `oxim run` under their own
    //! supervisor.

    use std::io::Write;
    use std::path::Path;
    use std::process::Command;

    use super::SERVICE_NAME;
    use crate::CliResult;

    const UNIT_PATH: &str = "/etc/systemd/system/oxim.service";

    fn systemctl(args: &[&str]) -> CliResult<()> {
        let status = Command::new("systemctl").args(args).status()?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("systemctl {} failed", args.join(" ")).into())
        }
    }

    pub(super) fn install(config: &Path, out: &mut impl Write) -> CliResult<()> {
        let unit = super::systemd_unit(&std::env::current_exe()?, config);
        std::fs::write(UNIT_PATH, unit)
            .map_err(|e| format!("cannot write {UNIT_PATH} (run as root): {e}"))?;
        systemctl(&["daemon-reload"])?;
        systemctl(&["enable", SERVICE_NAME])?;
        writeln!(out, "installed {UNIT_PATH}; the unit runs as user `oxim`")?;
        writeln!(
            out,
            "create it if needed: useradd --system --home-dir /var/lib/oxim oxim"
        )?;
        writeln!(out, "then start with `oxim service start`")?;
        Ok(())
    }

    pub(super) fn uninstall(out: &mut impl Write) -> CliResult<()> {
        let _ = systemctl(&["disable", "--now", SERVICE_NAME]);
        std::fs::remove_file(UNIT_PATH).map_err(|e| format!("cannot remove {UNIT_PATH}: {e}"))?;
        systemctl(&["daemon-reload"])?;
        writeln!(out, "removed {UNIT_PATH}")?;
        Ok(())
    }

    pub(super) fn start(out: &mut impl Write) -> CliResult<()> {
        systemctl(&["start", SERVICE_NAME])?;
        writeln!(out, "started {SERVICE_NAME}")?;
        Ok(())
    }

    pub(super) fn stop(out: &mut impl Write) -> CliResult<()> {
        systemctl(&["stop", SERVICE_NAME])?;
        writeln!(out, "stopped {SERVICE_NAME}")?;
        Ok(())
    }

    pub(super) fn run(config: &Path) -> CliResult<()> {
        // systemd runs `oxim run` directly; this entry exists for parity.
        let settings = crate::settings::Settings::load(config)?;
        let _guard = crate::logging::init(&settings.log)?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        runtime.block_on(crate::run::run(settings, crate::run::shutdown_signal()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn systemd_unit_runs_the_engine() {
        let unit = systemd_unit(Path::new("/usr/bin/oxim"), Path::new("/etc/oxim/oxim.yaml"));
        assert!(unit.contains("ExecStart=/usr/bin/oxim run --config /etc/oxim/oxim.yaml"));
        assert!(unit.contains("Restart=on-failure"));
    }
}
