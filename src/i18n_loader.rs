//! Runtime i18n backend, same shape as zedis.
//!
//! `i18n!` points at the empty `locales_stub/` so it embeds no translations
//! at compile time. The real `assets/locales/*.toml` are compressed by
//! rust-embed; [`LazyLocaleBackend`] inflates **one locale on first lookup**.

use rust_i18n::Backend;
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::OnceLock;

use crate::assets;

/// Bytes for `locales/en.toml` from the shared embed.
fn locale_file(locale: &str) -> Option<Cow<'static, [u8]>> {
    assets::get(&format!("locales/{locale}.toml"))
}

/// Per-locale lazy store: names are known up front, contents stay compressed
/// until the first `translate()` for that locale.
pub struct LazyLocaleBackend {
    locales: Vec<(String, OnceLock<HashMap<String, String>>)>,
}

impl LazyLocaleBackend {
    fn translations(&self, locale: &str) -> Option<&HashMap<String, String>> {
        let (name, cell) = self.locales.iter().find(|(name, _)| name == locale)?;
        Some(cell.get_or_init(|| parse_locale(name)))
    }
}

impl Backend for LazyLocaleBackend {
    fn available_locales(&self) -> Vec<Cow<'_, str>> {
        self.locales
            .iter()
            .map(|(name, _)| Cow::Borrowed(name.as_str()))
            .collect()
    }

    fn translate(&self, locale: &str, key: &str) -> Option<Cow<'_, str>> {
        self.translations(locale)?
            .get(key)
            .map(|value| Cow::Borrowed(value.as_str()))
    }

    fn messages_for_locale(&self, locale: &str) -> Option<Vec<(Cow<'_, str>, Cow<'_, str>)>> {
        let messages = self
            .translations(locale)?
            .iter()
            .map(|(key, value)| (Cow::Borrowed(key.as_str()), Cow::Borrowed(value.as_str())))
            .collect();
        Some(messages)
    }
}

/// Build the backend over embedded `locales/*.toml`. Only file *names* are
/// read here; contents stay compressed until first use.
pub fn runtime_backend() -> LazyLocaleBackend {
    let locales = assets::Assets::iter()
        .filter_map(|path| {
            path.strip_prefix("locales/")?
                .strip_suffix(".toml")
                .map(|locale| (locale.to_string(), OnceLock::new()))
        })
        .collect();
    LazyLocaleBackend { locales }
}

fn parse_locale(locale: &str) -> HashMap<String, String> {
    let mut flat = HashMap::new();
    let Some(bytes) = locale_file(locale) else {
        return flat;
    };
    let Ok(content) = std::str::from_utf8(&bytes) else {
        return flat;
    };
    let Ok(value) = toml::from_str::<toml::Value>(content) else {
        return flat;
    };
    flatten_keys(String::new(), &value, &mut flat);
    flat
}

fn flatten_keys(prefix: String, value: &toml::Value, out: &mut HashMap<String, String>) {
    match value {
        toml::Value::Table(map) => {
            for (key, child) in map {
                let next = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                flatten_keys(next, child, out);
            }
        }
        toml::Value::String(s) => {
            out.insert(prefix, s.clone());
        }
        toml::Value::Boolean(b) => {
            out.insert(prefix, b.to_string());
        }
        toml::Value::Integer(n) => {
            out.insert(prefix, n.to_string());
        }
        toml::Value::Float(n) => {
            out.insert(prefix, n.to_string());
        }
        toml::Value::Datetime(d) => {
            out.insert(prefix, d.to_string());
        }
        toml::Value::Array(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::runtime_backend;
    use rust_i18n::Backend;

    #[test]
    fn loads_en_and_zh() {
        let backend = runtime_backend();
        let locales = backend.available_locales();
        assert!(locales.iter().any(|l| l.as_ref() == "en"));
        assert!(locales.iter().any(|l| l.as_ref() == "zh"));
        assert_eq!(
            backend.translate("en", "common.quit").as_deref(),
            Some("Quit")
        );
        assert_eq!(
            backend.translate("zh", "common.quit").as_deref(),
            Some("退出")
        );
        assert!(backend.translate("en", "common.__missing__").is_none());
    }

    /// A key present in one locale and missing from another does not fail —
    /// `rust_i18n` silently serves the fallback, so the only symptom is one
    /// stray English string in an otherwise translated panel. That is not
    /// something review catches, hence this.
    #[test]
    fn every_locale_defines_the_same_keys() {
        let backend = runtime_backend();
        let keys = |locale: &str| -> std::collections::BTreeSet<String> {
            backend
                .messages_for_locale(locale)
                .unwrap_or_default()
                .into_iter()
                .map(|(k, _)| k.into_owned())
                .collect()
        };
        let en = keys("en");
        assert!(!en.is_empty(), "en should not be empty");
        for locale in backend.available_locales() {
            let other = keys(&locale);
            let missing: Vec<_> = en.difference(&other).collect();
            let extra: Vec<_> = other.difference(&en).collect();
            assert!(
                missing.is_empty() && extra.is_empty(),
                "locale {locale}: missing {missing:?}, unexpected {extra:?}"
            );
        }
    }

    #[test]
    fn parses_only_the_touched_locale() {
        let backend = runtime_backend();
        backend.available_locales();
        assert!(backend.locales.iter().all(|(_, cell)| cell.get().is_none()));
        assert!(backend.translate("zh", "common.quit").is_some());
        for (name, cell) in &backend.locales {
            assert_eq!(cell.get().is_some(), name == "zh", "locale {name}");
        }
    }
}
