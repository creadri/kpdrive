//! Translations for everything the user reads.
//!
//! Both binaries share this: the CLI and daemon because they print and notify,
//! the window because its labels come from here through the backend. Keeping
//! one runtime in Rust is what lets the wording the two front ends share be
//! translated once rather than twice.
//!
//! Log lines are deliberately not translated. They are a support tool, and a
//! report nobody can grep is worth less than one in the wrong language.

use gettext::{Catalog, ParseOptions};
use std::sync::OnceLock;

include!(concat!(env!("OUT_DIR"), "/catalogs.rs"));

static CATALOG: OnceLock<Catalog> = OnceLock::new();
/// The loaded language's plural rule. The catalog is told to ask this rather
/// than resolve the rule itself, which it does incorrectly for every language
/// with more than two forms.
static RULE: OnceLock<crate::plural::Rule> = OnceLock::new();

/// Handed to the catalog as its resolver. It has to be a plain function, so
/// the rule it reads is the one loaded at startup.
fn form_for(n: u64) -> usize {
    match RULE.get() {
        Some(rule) => rule.form(n),
        None if n == 1 => 0,
        None => 1,
    }
}

/// Loads the catalog for the language the environment asks for. Called once at
/// startup by each binary; anything unknown, and anything English, leaves the
/// source strings as they are.
pub fn init() {
    let Some(wanted) = language() else { return };
    // `fr_BE.UTF-8` should find fr_BE if we have it, and fr if we do not.
    let short = wanted.split('_').next().unwrap_or(&wanted).to_owned();
    for candidate in [wanted.as_str(), short.as_str()] {
        let Some((_, bytes, expression)) = CATALOGS.iter().find(|(lang, ..)| *lang == candidate) else { continue };
        if let Some(rule) = crate::plural::parse(expression) {
            let _ = RULE.set(rule);
        } else {
            eprintln!("kpdrive: plural rule for {candidate} not understood ({expression}); counting as English");
        }
        match ParseOptions::new().force_plural(form_for).parse(*bytes) {
            Ok(catalog) => {
                let _ = CATALOG.set(catalog);
                return;
            }
            // A catalog that will not parse is a build problem, not a reason to
            // fail: English is always a correct answer.
            Err(e) => eprintln!("kpdrive: translation catalog {candidate} is unusable: {e}"),
        }
    }
}

/// The language the environment asks for, as `fr` or `fr_BE`. `None` for the
/// C locale and for English, which need no catalog.
fn language() -> Option<String> {
    let raw = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|k| std::env::var(k).ok())
        .filter(|v| !v.is_empty())?;
    // `fr_BE.UTF-8@euro` is the full form; only the first part names a catalog.
    let name = raw.split(['.', '@']).next().unwrap_or(&raw);
    match name {
        "C" | "POSIX" | "" => None,
        name if name == "en" || name.starts_with("en_") => None,
        name => Some(name.to_owned()),
    }
}

/// One translated string. Returns the original when there is no catalog, which
/// is what makes English cost nothing.
pub fn t(message: &'static str) -> &'static str {
    match CATALOG.get() {
        Some(catalog) => catalog.gettext(message),
        None => message,
    }
}

/// The singular or plural form for `n`, chosen by the catalog's own rules, so
/// languages with more than two forms are counted properly.
pub fn tn(singular: &'static str, plural: &'static str, n: u64) -> &'static str {
    match CATALOG.get() {
        Some(catalog) => catalog.ngettext(singular, plural, n),
        None if n == 1 => singular,
        None => plural,
    }
}

/// As [`t`], for a string that is not a literal in this crate. The window's
/// labels arrive from QML as owned strings, so they cannot borrow.
pub fn lookup(message: &str) -> String {
    match CATALOG.get() {
        Some(catalog) => catalog.gettext(message).to_owned(),
        None => message.to_owned(),
    }
}

/// As [`tn`], for the same reason.
pub fn lookup_plural(singular: &str, plural: &str, n: u64) -> String {
    match CATALOG.get() {
        Some(catalog) => catalog.ngettext(singular, plural, n).to_owned(),
        None if n == 1 => singular.to_owned(),
        None => plural.to_owned(),
    }
}

/// Fills `{name}` placeholders in a translated string.
///
/// `format!` needs a literal, which a translation is not, so the values go in
/// afterwards. Named rather than positional, because a translator moving them
/// around is the whole point.
pub fn fill(template: &str, values: &[(&str, &str)]) -> String {
    let mut out = template.to_owned();
    for (name, value) in values {
        out = out.replace(&format!("{{{name}}}"), value);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The catalogs we ship, counted against what a speaker would say. This is
    /// what caught the crate resolving Arabic and Russian to one form each.
    #[test]
    fn shipped_catalogs_count_correctly() {
        let cases: &[(&str, &[(u64, usize)])] = &[
            ("fr", &[(0, 0), (1, 0), (2, 1), (1568, 1)]),
            ("de", &[(1, 0), (0, 1), (2, 1)]),
            ("ar", &[(0, 0), (1, 1), (2, 2), (3, 3), (11, 4), (100, 5), (1568, 4)]),
            ("ru", &[(1, 0), (2, 1), (5, 2), (21, 0), (1568, 2)]),
            ("pl", &[(1, 0), (2, 1), (5, 2), (22, 1)]),
            ("cs", &[(1, 0), (3, 1), (9, 2)]),
            ("ro", &[(1, 0), (2, 1), (20, 2)]),
            ("ja", &[(0, 0), (1, 0), (7, 0)]),
        ];
        for (lang, expected) in cases {
            let Some((_, _, expression)) = CATALOGS.iter().find(|(l, ..)| l == lang) else {
                panic!("{lang} is not shipped");
            };
            let rule = crate::plural::parse(expression).unwrap_or_else(|| panic!("{lang}: {expression} did not parse"));
            for (n, form) in *expected {
                assert_eq!(rule.form(*n), *form, "{lang} with n={n}");
            }
        }
    }

    #[test]
    fn english_costs_nothing() {
        assert_eq!(t("Sign out"), "Sign out", "no catalog means the source string");
        assert_eq!(tn("{n} photo", "{n} photos", 1), "{n} photo");
        assert_eq!(tn("{n} photo", "{n} photos", 3), "{n} photos");
    }

    #[test]
    fn placeholders_are_filled_by_name() {
        let out = fill("{folder} holds {n} items", &[("folder", "/tmp/x"), ("n", "3")]);
        assert_eq!(out, "/tmp/x holds 3 items");
        assert_eq!(fill("nothing to fill", &[("n", "1")]), "nothing to fill");
    }

    #[test]
    fn the_locale_variable_decides() {
        // Parsing only; the environment itself is process-wide and shared with
        // every other test, so it is not touched here.
        for (raw, wanted) in [
            ("fr_BE.UTF-8", Some("fr_BE")),
            ("fr", Some("fr")),
            ("nl_BE@euro", Some("nl_BE")),
            ("C", None),
            ("POSIX", None),
            ("en_GB.UTF-8", None),
        ] {
            let name = raw.split(['.', '@']).next().unwrap_or(raw);
            let got = match name {
                "C" | "POSIX" | "" => None,
                n if n == "en" || n.starts_with("en_") => None,
                n => Some(n),
            };
            assert_eq!(got, wanted, "for {raw}");
        }
    }
}
