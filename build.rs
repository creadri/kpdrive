//! Compiles the translation catalogs into the binary.
//!
//! Catalogs are a few kilobytes each, so embedding them means nothing to
//! install and nothing to find at runtime. `msgfmt` comes with gettext; without
//! it the build still works and every language falls back to English.

use std::{env, fs, path::PathBuf, process::Command};

/// The `plural=` half of a catalog's Plural-Forms header.
///
/// The header is the translation of the empty message id, and a long rule is
/// written across as many quoted lines as it needs, so the lines are joined
/// before the rule is cut out of them.
fn plural_rule(po: &str) -> Option<String> {
    let header: String = po
        .lines()
        .skip_while(|line| !line.starts_with("msgstr \"\""))
        .skip(1)
        .take_while(|line| line.starts_with('"'))
        .map(|line| line.trim().trim_matches('"'))
        .collect();
    let rest = header.split("plural=").nth(1)?;
    let rule = &rest[..rest.find(';').unwrap_or(rest.len())];
    Some(rule.replace("\\n", "").trim().to_owned())
}

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
                // The rule comes from the catalog itself rather than a table
                // here, so a new language needs no code change.
                let rule = fs::read_to_string(&po)
                    .ok()
                    .and_then(|text| plural_rule(&text))
                    .unwrap_or_else(|| "n != 1".to_owned());
                arms.push_str(&format!("    ({lang:?}, include_bytes!({:?}), {rule:?}),\n", mo.display().to_string()));
                languages.push(lang);
            }
            _ => println!("cargo:warning=could not compile po/{lang}.po; that language falls back to English"),
        }
    }
    languages.sort();
    fs::write(
        out.join("catalogs.rs"),
        format!(
            "/// Compiled catalogs and their plural rules, by language: {}\nstatic CATALOGS: &[(&str, &[u8], &str)] = &[\n{arms}];\n",
            languages.join(" ")
        ),
    )
    .expect("write catalogs.rs");
}
