//! Keeps the packaging in `deploy/` consistent with the program: the
//! packaged systemd unit is the one `oxim service unit` generates, every
//! shipped configuration and example channel validates, and every file the
//! deb and rpm metadata names exists.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::Command;

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn deploy(path: &str) -> PathBuf {
    repository().join("deploy").join(path)
}

fn oxim(args: &[&str]) -> (bool, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_oxim"))
        .args(args)
        .output()
        .unwrap();
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    (output.status.success(), text)
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
        .replace("\r\n", "\n")
}

#[test]
fn the_packaged_unit_is_the_generated_unit() {
    let (ok, generated) = oxim(&["-c", "/etc/oxim/oxim.yaml", "service", "unit"]);
    assert!(ok, "{generated}");
    // The generator writes the path of the running executable.
    let generated: String = generated
        .lines()
        .map(|line| match line.strip_prefix("ExecStart=") {
            Some(rest) => {
                let arguments = rest.find(" run --config ").unwrap();
                format!("ExecStart=/usr/bin/oxim{}\n", &rest[arguments..])
            }
            None => format!("{line}\n"),
        })
        .collect();
    assert_eq!(generated, read(&deploy("systemd/oxim.service")));
}

/// Copies `files` (source, name) into `dir`, dropping a `.example` suffix.
fn copy_into(dir: &Path, files: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    for entry in std::fs::read_dir(files).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy();
        let name = name.strip_suffix(".example").unwrap_or(&name);
        std::fs::copy(&path, dir.join(name)).unwrap();
    }
}

/// Validates `config` after replacing absolute paths, with the channel and
/// table files of `channels` and `tables`.
fn validates(config: &Path, replacements: &[(&str, &str)], channels: &Path, tables: &Path) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_string_lossy().replace('\\', "/");
    let mut text = read(config);
    for (from, to) in replacements {
        text = text.replace(from, &to.replace("{root}", &root));
    }
    let path = dir.path().join("oxim.yaml");
    std::fs::write(&path, text).unwrap();
    copy_into(&dir.path().join("channels"), channels);
    copy_into(&dir.path().join("tables"), tables);
    let (ok, output) = oxim(&["-c", &path.to_string_lossy(), "validate"]);
    assert!(ok, "{}:\n{output}", config.display());
    let files = std::fs::read_dir(channels).unwrap().count();
    assert_eq!(
        output.lines().filter(|line| line.starts_with("ok")).count(),
        files,
        "{}:\n{output}",
        config.display()
    );
}

#[test]
fn shipped_configurations_and_examples_validate() {
    let examples = deploy("examples/channels");
    let tables = deploy("examples/tables");
    validates(
        &deploy("linux/oxim.yaml"),
        &[
            ("/etc/oxim/channels", "{root}/channels"),
            ("/etc/oxim/tables", "{root}/tables"),
            ("/var/lib/oxim", "{root}/data"),
        ],
        &examples,
        &tables,
    );
    validates(&deploy("windows/oxim.yaml"), &[], &examples, &tables);
    validates(
        &deploy("docker/demo/oxim.yaml"),
        &[("/var/lib/oxim", "{root}/data")],
        &deploy("docker/demo/channels"),
        &tables,
    );
}

#[test]
fn the_windows_installer_ships_every_example() {
    let wxs = read(&deploy("windows/oxim.wxs"));
    for directory in ["examples/channels", "examples/tables"] {
        for entry in std::fs::read_dir(deploy(directory)).unwrap() {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            let source = format!("{}\\{name}\"", directory.replace('/', "\\"));
            assert!(wxs.contains(&source), "oxim.wxs does not install {name}");
        }
    }
}

#[test]
fn deployment_yaml_parses() {
    for path in [
        "deploy/docker/compose.yaml",
        "deploy/helm/oxim/Chart.yaml",
        "deploy/helm/oxim/values.yaml",
        ".github/workflows/release.yml",
    ] {
        let text = read(&repository().join(path));
        let value: serde_json::Value =
            serde_saphyr::from_str(&text).unwrap_or_else(|e| panic!("{path}: {e}"));
        assert!(value.is_object(), "{path}");
    }
}

#[test]
fn package_assets_exist() {
    let manifest = read(&Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"));
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut checked = 0;
    for source in manifest
        .split('"')
        .filter(|part| part.starts_with("../../") || *part == "README.md")
    {
        let path = crate_dir.join(source);
        match source.rsplit_once("/*") {
            Some((directory, extension)) => {
                let extension = extension.trim_start_matches('.');
                let found = std::fs::read_dir(crate_dir.join(directory))
                    .unwrap_or_else(|e| panic!("{source}: {e}"))
                    .filter_map(Result::ok)
                    .any(|entry| {
                        extension.is_empty()
                            || entry.path().extension().is_some_and(|e| e == extension)
                    });
                assert!(found, "{source} matches no file");
            }
            None => assert!(path.exists(), "{source} does not exist"),
        }
        checked += 1;
    }
    assert!(checked > 20, "only {checked} package assets found");
}
