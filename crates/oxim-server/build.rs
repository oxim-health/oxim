//! Embeds the built web UI (`ui/dist`, or the directory named by
//! `OXIM_UI_DIST`) in the library when the `embedded-ui` feature is on.
//!
//! The generated `ui_assets.rs` lists every file with `include_bytes!`, so
//! the files become compile-time dependencies. When the UI has not been
//! built, the list is empty and the server shows a page explaining how to
//! build it; Node is never needed to compile OXIM.

use std::io::Write;
use std::path::{Path, PathBuf};

fn collect(root: &Path, dir: &Path, files: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            collect(root, &path, files);
        } else if let Ok(relative) = path.strip_prefix(root) {
            let name = relative
                .components()
                .map(|part| part.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            files.push((name, path));
        }
    }
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=OXIM_UI_DIST");
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap_or_default());
    let dist = std::env::var_os("OXIM_UI_DIST")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest.join("../../ui/dist"));
    let mut files = Vec::new();
    if std::env::var_os("CARGO_FEATURE_EMBEDDED_UI").is_some() {
        if dist.is_dir() {
            println!("cargo:rerun-if-changed={}", dist.display());
            if dist.join("index.html").is_file() {
                collect(&dist, &dist, &mut files);
                files.sort();
            }
        } else if let Some(parent) = dist.parent().filter(|parent| parent.is_dir()) {
            // Watching a missing path would make Cargo rebuild on every run;
            // watching the parent picks up a UI that is built later.
            println!("cargo:rerun-if-changed={}", parent.display());
        }
    }
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap_or_default());
    let mut code = String::from(
        "/// The embedded web UI files, sorted by path.\n\
         pub(crate) static ASSETS: &[(&str, &[u8])] = &[\n",
    );
    for (name, path) in &files {
        let path = path.to_string_lossy();
        code.push_str(&format!("    ({name:?}, include_bytes!({path:?})),\n"));
    }
    code.push_str("];\n");
    let target = out.join("ui_assets.rs");
    let unchanged = std::fs::read_to_string(&target).is_ok_and(|old| old == code);
    if !unchanged && let Ok(mut file) = std::fs::File::create(&target) {
        let _ = file.write_all(code.as_bytes());
    }
}
