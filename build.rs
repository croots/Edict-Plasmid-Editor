//! Embeds every base16 scheme under `themes/base16/` so they ship inside the binary.

use std::{env, fs, path::PathBuf};

fn main() {
    let dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("themes/base16");
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut files: Vec<PathBuf> = fs::read_dir(&dir)
        .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).collect())
        .unwrap_or_default();
    files.retain(|p| matches!(p.extension().and_then(|e| e.to_str()), Some("yaml" | "yml")));
    files.sort();

    let mut out = String::from("pub const BUNDLED: &[(&str, &str)] = &[\n");
    for f in &files {
        let slug = f.file_stem().unwrap().to_string_lossy();
        out.push_str(&format!("    ({slug:?}, include_str!({:?})),\n", f.display().to_string()));
    }
    out.push_str("];\n");
    fs::write(PathBuf::from(env::var("OUT_DIR").unwrap()).join("bundled_themes.rs"), out).unwrap();
}
