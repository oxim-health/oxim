# Installing OXIM on Windows

OXIM runs as the Windows service `oxim` (display name "OXIM Integration Engine"). The MSI installer exists for x64 and ARM64 and supports Windows 10, Windows 11 and Windows Server 2016 or later. For machines without network access, the [offline bundle](offline.md) is an alternative; the MSI itself also installs offline.

## Install

Run `oxim-<version>-windows-x64.msi` and follow the wizard, or install silently from an elevated prompt:

```bat
msiexec /i oxim-<version>-windows-x64.msi /qn /l*v oxim-install.log
```

The installer:

- installs `oxim.exe`, the licenses and example channels to `%ProgramFiles%\OXIM` and adds it to the system `PATH`;
- creates `%ProgramData%\OXIM` with `oxim.yaml`, `channels`, `tables`, `data` and `logs`, readable only by SYSTEM and Administrators because it holds clinical messages and the channel files the service executes;
- registers the service `oxim` (automatic start, LocalSystem account) as `oxim.exe service run --config "%ProgramData%\OXIM\oxim.yaml"`, the same registration `oxim service install` performs;
- does not start the service, because no channel is configured yet.

## Configure and start

From an elevated prompt:

```bat
copy "%ProgramFiles%\OXIM\examples\channels\mllp-archive.yaml.example" "%ProgramData%\OXIM\channels\mllp-archive.yaml"
oxim -c "%ProgramData%\OXIM\oxim.yaml" validate
oxim service start
```

The service writes daily log files to `%ProgramData%\OXIM\logs`. Channel files are picked up while the service runs; changes to `oxim.yaml` need `oxim service stop` and `oxim service start`. Open the listener ports of your channels in Windows Defender Firewall for the program `%ProgramFiles%\OXIM\oxim.exe`.

Serial analyzers are configured with the port name, for example `COM3`.

## First user

The web UI and REST API start once a user exists. From an elevated prompt, create the first administrator (the password is asked for without echo) and restart the service:

```bat
oxim -c "%ProgramData%\OXIM\oxim.yaml" users create-admin --username admin
oxim service stop
oxim service start
```

Then open `http://127.0.0.1:8080`. For access from other machines set `server.listen` together with `server.tls` in `oxim.yaml`.

## Service account

The service runs as LocalSystem, like `oxim service install`. To run it with fewer privileges, create a dedicated account (or use the virtual account `NT SERVICE\oxim`), grant it full control of `%ProgramData%\OXIM` and set it in the service properties (`services.msc`, Log On tab).

## Upgrade

Run the new MSI. The service is stopped, the program is replaced, and the configuration, channel files and data are kept. Start the service again with `oxim service start`. Back up `%ProgramData%\OXIM\data` before upgrading.

## Remove

Remove OXIM in Settings > Apps, or:

```bat
msiexec /x oxim-<version>-windows-x64.msi /qn
```

The service and program are removed. `%ProgramData%\OXIM` (configuration, channels, tables, messages and logs) is kept; delete it manually when it is no longer needed.

## Building the MSI

The installer is defined in [`deploy/windows/oxim.wxs`](../../deploy/windows/oxim.wxs) and built with the WiX Toolset v5:

```powershell
dotnet tool install --global wix --version 5.0.2
cargo build --release --locked -p oxim
.\deploy\windows\build-msi.ps1 -Version 1.0.0
```
