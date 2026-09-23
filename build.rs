//! Compiles the translation catalogs into the binary.
//!
//! Catalogs are a few kilobytes each, so embedding them means nothing to
//! install and nothing to find at runtime. `msgfmt` comes with gettext; without
//! it the build still works and every language falls back to English.

use std::{env, fs, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=po");
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let mut arms = String::new();
    let mut languages: Vec<String> = Vec::new();
    for entry in fs::read_dir("po").into_iter().flatten().flatten() {
        let po = entry.path();
        if po.extension().map(|e| e != "po").unwrap_or(true) {
            continue;
        }
        let lang = po.file_stem().expect("a name before .po").to_string_lossy().into_owned();
        let mo = out.join(format!("{lang}.mo"));
        match Command::new("msgfmt").arg("-o").arg(&mo).arg(&po).status() {
            Ok(status) if status.success() => {
                arms.push_str(&format!("    ({lang:?}, include_bytes!({:?})),\n", mo.display().to_string()));
                languages.push(lang);
            }
            _ => println!("cargo:warning=could not compile po/{lang}.po; that language falls back to English"),
        }
    }
    languages.sort();
    fs::write(
        out.join("catalogs.rs"),
        format!("/// Compiled catalogs, by language: {}\nstatic CATALOGS: &[(&str, &[u8])] = &[\n{arms}];\n", languages.join(" ")),
    )
    .expect("write catalogs.rs");
}
