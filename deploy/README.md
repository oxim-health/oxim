# deploy

Machine-readable deployment artifacts for OXIM. Installation guides for people are in [`docs/install`](../docs/install/README.md).

| Path | Contents | Guide |
|---|---|---|
| [`docker/`](docker) | `Dockerfile` (engine image and the `sim` target), `compose.yaml` demo with simulated analyzer and LIS | [Docker](../docs/install/docker.md) |
| [`helm/oxim/`](helm/oxim) | Helm chart: single-pod StatefulSet with a persistent SQLite store | [Kubernetes](../docs/install/kubernetes.md) |
| [`systemd/`](systemd) | `oxim.service` (identical to `oxim service unit`), the packaging drop-in, `sysusers.d` and `tmpfiles.d` files | [Linux](../docs/install/linux.md) |
| [`linux/oxim.yaml`](linux/oxim.yaml) | configuration installed by the deb and rpm packages and the Linux offline bundle | [Linux](../docs/install/linux.md) |
| [`debian/`](debian), [`rpm/`](rpm) | package maintainer scripts; the package metadata is in [`crates/oxim/Cargo.toml`](../crates/oxim/Cargo.toml) | [Linux](../docs/install/linux.md) |
| [`windows/`](windows) | WiX installer definition, its build script and the Windows configuration | [Windows](../docs/install/windows.md) |
| [`offline/`](offline) | offline bundle builders and the install scripts they include | [Offline](../docs/install/offline.md) |
| [`examples/`](examples) | example channels and tables shipped with every package | |

## Building packages

```sh
cargo build --release --locked -p oxim

cargo install cargo-deb cargo-generate-rpm
cargo deb -p oxim --no-build
cargo generate-rpm -p crates/oxim

deploy/offline/build-bundle.sh --version 1.0.0 --target x86_64-unknown-linux-gnu --binary target/release/oxim
docker build -f deploy/docker/Dockerfile -t oxim .
helm package deploy/helm/oxim
```

```powershell
.\deploy\windows\build-msi.ps1 -Version 1.0.0
.\deploy\offline\build-bundle.ps1 -Version 1.0.0 -Target x86_64-pc-windows-msvc -Binary target\release\oxim.exe
```

Release builds for every platform, with checksums, an SBOM and Sigstore signatures, run in [`.github/workflows/release.yml`](../.github/workflows/release.yml) when a version tag is pushed.

`crates/oxim/tests/deploy.rs` keeps these files consistent with the program: the packaged unit must equal `oxim service unit`, every shipped configuration and example channel must pass `oxim validate`, and every file named in the package metadata must exist.
