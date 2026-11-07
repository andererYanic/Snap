use std::io::Write;

fn main() {
    // Alle Sprachdateien aus ../locales einbetten (neue Sprache = neue Datei, kein Code).
    let dir = std::path::Path::new("../locales");
    println!("cargo:rerun-if-changed=../locales");
    let mut entries: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| {
                    let p = e.path();
                    (p.extension()? == "json").then(|| p.file_stem()?.to_str().map(str::to_string))?
                })
                .collect()
        })
        .unwrap_or_default();
    entries.sort();
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("locales.rs");
    let mut f = std::fs::File::create(out).unwrap();
    writeln!(f, "pub const LOCALES: &[(&str, &str)] = &[").unwrap();
    for code in &entries {
        let path = std::fs::canonicalize(dir.join(format!("{code}.json"))).unwrap();
        println!("cargo:rerun-if-changed={}", path.display());
        writeln!(f, "    ({code:?}, include_str!({:?})),", path.display().to_string()).unwrap();
    }
    writeln!(f, "];").unwrap();
    tauri_build::build()
}
