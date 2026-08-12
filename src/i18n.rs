//! Process locale: detect the OS language, pin rust-i18n, look up keys.

use rust_i18n::t;

/// Supported locales. Anything else (ja, fr, `C`) falls back to English.
const SUPPORTED: &[&str] = &["en", "zh"];

/// Map `zh-Hans-CN` / `en_US` / `zh` onto [`SUPPORTED`], else `"en"`.
pub fn detect() -> &'static str {
    let Some(raw) = sys_locale::get_locale() else {
        return "en";
    };
    let lang = raw
        .split(['-', '_'])
        .next()
        .unwrap_or(raw.as_str())
        .to_ascii_lowercase();
    if SUPPORTED.contains(&lang.as_str()) {
        // `'static` from the table, not from `lang`.
        if lang == "zh" { "zh" } else { "en" }
    } else {
        "en"
    }
}

/// Pin rust-i18n to the system language. Call once at startup, before the
/// first `t!` / tray menu is built.
pub fn init() {
    rust_i18n::set_locale(detect());
}

/// Look up a key in the active locale, falling back to English.
pub fn tr(key: &str) -> String {
    t!(key).to_string()
}
