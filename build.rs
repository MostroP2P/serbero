//! Embeds every message catalog, `messages/<code>.toml`, at build time
//! (`docs/spec.md` §7.7). Adding a language is adding a file: no code names
//! one.

use std::fmt::Write as _;
use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=messages");
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into());
    let dir = Path::new(&manifest).join("messages");
    let mut codes: Vec<String> = std::fs::read_dir(&dir)
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok().map(|e| e.path()))
                .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
                .filter_map(|path| Some(path.file_stem()?.to_str()?.to_owned()))
                .collect()
        })
        .unwrap_or_default();
    codes.sort();

    let mut out = String::from("/// `(code, file contents)` for every embedded catalog.\n");
    out.push_str("pub static EMBEDDED: &[(&str, &str)] = &[\n");
    for code in &codes {
        let path = dir.join(format!("{code}.toml"));
        println!("cargo:rerun-if-changed={}", path.display());
        let _ = writeln!(
            out,
            "    ({code:?}, include_str!({:?})),",
            path.display().to_string()
        );
    }
    out.push_str("];\n");

    let out_dir = std::env::var("OUT_DIR").unwrap_or_else(|_| ".".into());
    if let Err(e) = std::fs::write(Path::new(&out_dir).join("catalogs.rs"), out) {
        panic!("cannot write the embedded catalog list: {e}");
    }
}
