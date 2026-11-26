//! Embeds the built web app (`apps/web/dist`) into the binary.
//!
//! Generates a table of `include_bytes!` entries, one per file. A missing or
//! empty `dist` gives an empty table, so the crate builds before the web app
//! does and the server answers with a "not built" page instead.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn main() {
    let manifest_dir = PathBuf::from(env("CARGO_MANIFEST_DIR"));
    let dist = manifest_dir.join("../../apps/web/dist");
    // While `dist` is missing cargo reruns this script on every build, which
    // is how the first `vp build` gets noticed.
    println!("cargo:rerun-if-changed={}", dist.display());

    let mut files = Vec::new();
    collect(&dist, &dist, &mut files);
    files.sort();

    let mut table = String::from("&[\n");
    for (name, path) in &files {
        // `{:?}` of a str is a valid Rust string literal.
        let _ = writeln!(table, "    ({name:?}, include_bytes!({path:?})),");
    }
    table.push_str("]\n");

    let out = PathBuf::from(env("OUT_DIR")).join("web_assets.rs");
    std::fs::write(&out, table)
        .unwrap_or_else(|err| panic!("cannot write {}: {err}", out.display()));
}

/// Lists the files under `dir` as (path relative to `root` with `/`, absolute path).
fn collect(root: &Path, dir: &Path, files: &mut Vec<(String, String)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(root, &path, files);
            continue;
        }
        let Ok(relative) = path.strip_prefix(root) else {
            continue;
        };
        let name: Option<Vec<&str>> = relative
            .components()
            .map(|part| part.as_os_str().to_str())
            .collect();
        // A name that is not UTF-8 cannot be requested over HTTP as it is; skip it.
        if let (Some(name), Some(path)) = (name, path.to_str()) {
            files.push((name.join("/"), path.to_owned()));
        }
    }
}

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("cargo always sets {name}"))
}
