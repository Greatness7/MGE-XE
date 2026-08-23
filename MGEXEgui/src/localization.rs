use std::borrow::Cow;

use rust_i18n::{available_locales, set_locale, t};

pub const AUTO_LOCALE: &str = "auto";
pub const FALLBACK_LOCALE: &str = "en";
const LOCALE_OVERRIDE: &str = "MGEGUI_LOCALE";

pub fn initialize() {
    let locale = override_locale()
        .or_else(|| resolve_system_locale(sys_locale::get_locale().as_deref()))
        .unwrap_or_else(|| FALLBACK_LOCALE.into());
    set_locale(&locale);
}

pub fn apply_saved_locale(saved: &mut String) {
    let (normalized, selected) = normalize_saved_locale(saved);
    *saved = normalized;

    let locale = override_locale()
        .or(selected)
        .or_else(|| resolve_system_locale(sys_locale::get_locale().as_deref()))
        .unwrap_or_else(|| FALLBACK_LOCALE.into());
    set_locale(&locale);
}

fn normalize_saved_locale(saved: &str) -> (String, Option<String>) {
    if saved == AUTO_LOCALE {
        return (AUTO_LOCALE.into(), None);
    }
    match available_locale(saved) {
        Some(locale) => (locale.clone(), Some(locale)),
        None => (AUTO_LOCALE.into(), None),
    }
}

pub fn available_locale_codes() -> Vec<Cow<'static, str>> {
    available_locales!()
}

pub fn language_name(locale: &str) -> String {
    t!("language.name", locale = locale).into_owned()
}

pub fn automatic_name() -> String {
    t!("language.automatic").into_owned()
}

fn override_locale() -> Option<String> {
    std::env::var(LOCALE_OVERRIDE).ok().and_then(|locale| reduce_locale(&locale))
}

fn resolve_system_locale(locale: Option<&str>) -> Option<String> {
    locale.and_then(reduce_locale)
}

fn available_locale(locale: &str) -> Option<String> {
    available_locale_codes()
        .into_iter()
        .find(|available| available.eq_ignore_ascii_case(locale))
        .map(Cow::into_owned)
}

fn reduce_locale(locale: &str) -> Option<String> {
    let normalized = locale.trim().replace('_', "-");
    available_locale(&normalized).or_else(|| {
        normalized
            .split_once('-')
            .and_then(|(language, _)| available_locale(language))
    })
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::*;

    const INTENTIONAL_FALLBACKS: &[&str] = &["tests.english_fallback"];

    fn catalog(source: &str) -> BTreeMap<String, String> {
        fn flatten(prefix: &str, value: &toml::Value, entries: &mut BTreeMap<String, String>) {
            match value {
                toml::Value::Table(table) => {
                    for (key, value) in table {
                        let path = if prefix.is_empty() {
                            key.clone()
                        } else {
                            format!("{prefix}.{key}")
                        };
                        flatten(&path, value, entries);
                    }
                }
                toml::Value::String(value) => {
                    entries.insert(prefix.to_owned(), value.clone());
                }
                _ => {}
            }
        }

        let value: toml::Value = toml::from_str(source).expect("catalog must be valid TOML");
        let mut entries = BTreeMap::new();
        flatten("", &value, &mut entries);
        entries
    }

    fn placeholders(value: &str) -> BTreeSet<&str> {
        let mut placeholders = BTreeSet::new();
        let mut rest = value;
        while let Some(start) = rest.find("%{") {
            rest = &rest[start + 2..];
            let Some(end) = rest.find('}') else {
                break;
            };
            placeholders.insert(&rest[..end]);
            rest = &rest[end + 1..];
        }
        placeholders
    }

    #[test]
    fn regional_locale_reduces_to_available_language() {
        assert_eq!(reduce_locale("fr-FR"), Some("fr".into()));
        assert_eq!(reduce_locale("pl_PL"), Some("pl".into()));
    }

    #[test]
    fn unavailable_locale_has_no_match() {
        assert_eq!(reduce_locale("zz-ZZ"), None);
    }

    #[test]
    fn invalid_saved_locale_normalizes_to_automatic() {
        assert_eq!(normalize_saved_locale("not-a-locale"), (AUTO_LOCALE.into(), None));
    }

    #[test]
    fn every_catalog_has_a_self_localized_language_name() {
        for locale in available_locale_codes() {
            assert_ne!(language_name(&locale), "language.name");
            assert!(!language_name(&locale).is_empty());
        }
    }

    #[test]
    fn missing_secondary_entry_falls_back_to_english() {
        for locale in ["fr", "pl", "ru"] {
            assert_eq!(t!("tests.english_fallback", locale = locale), "English fallback");
        }
    }

    #[test]
    fn named_interpolation_uses_catalog_template() {
        assert_eq!(t!("tests.named_interpolation", locale = "en", value = 7), "Value 7");
    }

    #[test]
    fn secondary_catalogs_are_complete_and_placeholder_safe() {
        let english = catalog(include_str!("../locales/en.toml"));
        let intentional_fallbacks = INTENTIONAL_FALLBACKS.iter().copied().collect::<BTreeSet<_>>();

        for (locale, source) in [
            ("fr", include_str!("../locales/fr.toml")),
            ("pl", include_str!("../locales/pl.toml")),
            ("ru", include_str!("../locales/ru.toml")),
        ] {
            let localized = catalog(source);
            let missing = english
                .keys()
                .filter(|key| !localized.contains_key(*key))
                .map(String::as_str)
                .collect::<BTreeSet<_>>();
            assert_eq!(missing, intentional_fallbacks, "{locale} has unexpected catalog gaps");

            let extra = localized.keys().filter(|key| !english.contains_key(*key)).collect::<Vec<_>>();
            assert!(extra.is_empty(), "{locale} has unknown keys: {extra:?}");

            for (key, translation) in &localized {
                assert_eq!(
                    placeholders(&english[key]),
                    placeholders(translation),
                    "{locale}.{key} does not preserve named placeholders"
                );
            }
        }
    }
}
