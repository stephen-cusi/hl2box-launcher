//! UI translations loaded from JSON language files.
//!
//! Format: one flat JSON object per locale, `key` -> `string`, with
//! `{{name}}` placeholders - the shape every web i18n editor (i18next,
//! Lokalise, Crowdin, ...) opens directly. Two locales ship compiled-in
//! as the built-in fallback:
//!
//! ```text
//! lang/zh-CN.json
//! lang/en.json
//! ```
//!
//! At startup every `<locale>.json` next to the executable (or beside the
//! source tree during development) is loaded on top of the built-ins, so
//! new locales and typo fixes are just dropped-in files - no rebuild.
//! Lookup order: chosen locale -> en -> the key itself (visible in the
//! UI, never a blank label).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const FALLBACK_LOCALE: &str = "en";
pub const BUILT_IN_LOCALES: [(&str, &str); 2] = [
    ("zh-CN", include_str!("../lang/zh-CN.json")),
    ("en", include_str!("../lang/en.json")),
];

pub struct Catalog {
    /// locale name -> key -> string
    tables: HashMap<String, HashMap<String, String>>,
    /// Loaded order, for the language picker.
    pub locales: Vec<String>,
}

impl Catalog {
    /// Built-in locales plus every `<locale>.json` found in `dir`.
    pub fn load(dir: &Path) -> Catalog {
        let mut tables: HashMap<String, HashMap<String, String>> = HashMap::new();
        let mut order: Vec<String> = Vec::new();

        let insert = |tables: &mut HashMap<String, HashMap<String, String>>,
                          order: &mut Vec<String>,
                          locale: &str,
                          text: &str| {
            if let Ok(map) = serde_json::from_str::<HashMap<String, String>>(text) {
                if map.is_empty() {
                    return;
                }
                // An external file for a locale we already bundle MERGES into
                // the built-in table instead of replacing it: a stale lang/
                // file from an older build would otherwise hide every key
                // added since and the UI falls back to raw key names.
                match tables.get_mut(locale) {
                    Some(bundled) => {
                        for (k, v) in map {
                            bundled.insert(k, v);
                        }
                    }
                    None => {
                        tables.insert(locale.to_string(), map);
                    }
                }
                if !order.iter().any(|l| l == locale) {
                    order.push(locale.to_string());
                }
            }
        };

        for (locale, text) in BUILT_IN_LOCALES {
            insert(&mut tables, &mut order, locale, text);
        }

        if let Ok(rd) = std::fs::read_dir(dir) {
            let mut files: Vec<PathBuf> = rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.extension()
                        .and_then(|e| e.to_str())
                        .map(|e| e.eq_ignore_ascii_case("json"))
                        .unwrap_or(false)
                })
                .collect();
            files.sort();
            for path in files {
                let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                if let Ok(text) = std::fs::read_to_string(&path) {
                    insert(&mut tables, &mut order, stem, &text);
                }
            }
        }

        // The fallback must always exist.
        if !order.iter().any(|l| l == FALLBACK_LOCALE) {
            order.push(FALLBACK_LOCALE.into());
            tables.insert(FALLBACK_LOCALE.into(), HashMap::new());
        }

        Catalog {
            tables,
            locales: order,
        }
    }

    pub fn has(&self, locale: &str) -> bool {
        self.tables.contains_key(locale)
    }

    /// Translate with fallback: locale -> en -> key. The result lives as
    /// long as the catalog (or the key literal), whichever the caller needs.
    pub fn tr<'s>(&'s self, locale: &str, key: &'s str) -> &'s str {
        if let Some(t) = self.tables.get(locale) {
            if let Some(s) = t.get(key) {
                return s;
            }
        }
        if let Some(t) = self.tables.get(FALLBACK_LOCALE) {
            if let Some(s) = t.get(key) {
                return s;
            }
        }
        key
    }

    /// OS locale -> one of the loaded locales: exact match, then the
    /// language part (`zh-TW` -> `zh`), then the built-in that shares the
    /// language, else the fallback.
    pub fn pick_system_locale(&self) -> String {
        let Some(sys) = sys_locale::get_locale() else {
            return FALLBACK_LOCALE.into();
        };
        let sys = sys.replace('_', "-").to_lowercase();
        if self.has(&sys) {
            return sys;
        }
        let lang = sys.split('-').next().unwrap_or("").to_string();
        if self.has(&lang) {
            return lang;
        }
        // Try the loaded locales that start with the same language tag.
        for l in &self.locales {
            if l.to_lowercase().starts_with(&lang) {
                return l.clone();
            }
        }
        FALLBACK_LOCALE.into()
    }
}

/// Format `{{name}}` placeholders in a translated string.
pub fn format(template: &str, args: &[(&str, &str)]) -> String {
    let mut s = template.to_string();
    for (k, v) in args {
        s = s.replace(&format!("{{{{{k}}}}}"), v);
    }
    s
}

#[cfg(test)]
mod key_tests {
    use std::collections::HashSet;

    fn keys(json: &str) -> HashSet<String> {
        let map: std::collections::HashMap<String, String> = serde_json::from_str(json).unwrap();
        map.into_keys().collect()
    }

    /// Every translation key a file references as a literal must exist in
    /// both bundled languages, and the two languages must share one key set.
    #[test]
    fn locales_match_and_cover_used_keys() {
        let en = keys(include_str!("../lang/en.json"));
        let zh = keys(include_str!("../lang/zh-CN.json"));
        assert_eq!(en, zh, "en and zh-CN key sets differ");

        let sources = [
            include_str!("state.rs"),
            include_str!("explorer.rs"),
            include_str!("logview.rs"),
            include_str!("textview.rs"),
        ];
        let mut missing = Vec::new();
        for src in sources {
            let bytes = src.as_bytes();
            for (i, _) in src.match_indices("t(\"").chain(src.match_indices("tf(\"")).chain(src.match_indices("tl(\"")) {
                // the character before the call name must not extend an identifier
                // (so `split("` or `start("` do not count)
                let name_start = if src[i..].starts_with("tf(") || src[i..].starts_with("tl(") { i } else { i };
                let prev = if name_start == 0 { b' ' } else { bytes[name_start - 1] };
                if prev.is_ascii_alphanumeric() || prev == b'_' {
                    continue;
                }
                let rest = &src[i..];
                let q = rest.find('"').unwrap() + 1;
                let end = rest[q..].find('"').unwrap();
                let key = &rest[q..q + end];
                if key.contains('{') || key.is_empty() {
                    continue;
                }
                if !en.contains(key) {
                    missing.push(key.to_string());
                }
            }
        }
        assert!(missing.is_empty(), "keys used in code but absent from lang files: {missing:?}");
    }
}
