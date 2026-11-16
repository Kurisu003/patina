//! Embeds assets/icons/*.svg into the binary so release builds are self-contained.
use std::{env, fs, path::Path};

fn main() {
    let dir = Path::new(&env::var("CARGO_MANIFEST_DIR").unwrap()).join("assets/icons");
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut names: Vec<_> = fs::read_dir(&dir)
        .expect("assets/icons")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "svg"))
        .collect();
    names.sort();
    let mut out = String::from("pub static ICONS: &[(&str, &[u8])] = &[\n");
    for p in names {
        let stem = p.file_stem().unwrap().to_string_lossy();
        out.push_str(&format!("    ({:?}, include_bytes!({:?})),\n", stem, p.display().to_string()));
    }
    out.push_str("];\n");
    fs::write(Path::new(&env::var("OUT_DIR").unwrap()).join("icons.rs"), out).unwrap();
}
