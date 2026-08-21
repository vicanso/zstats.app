//! Process locale: detect the OS language, pin rust-i18n, look up keys.

use crate::prefs;
use rust_i18n::t;
use std::sync::atomic::{AtomicBool, Ordering};

/// Supported locales. Anything else (ja, fr, `C`) falls back to English.
const SUPPORTED: &[&str] = &["en", "zh"];

/// Mirror of "the active locale draws its words in Han characters",
/// kept by [`init`]. An atomic rather than a `rust_i18n::locale()`
/// string compare because [`crate::theme::tiny_label`] asks on every
/// small label of every repaint, at the collector's cadence.
static CJK: AtomicBool = AtomicBool::new(false);

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

/// Pin rust-i18n to the preferred language: the `app.toml` override when
/// one is set, the system language otherwise. Call once at startup before
/// the first `t!` / tray menu is built, and again whenever the preference
/// changes — the whole panel repaints per tick, so re-pinning is all a
/// live switch needs.
pub fn init() {
    let locale = prefs::language().locale().unwrap_or_else(detect);
    CJK.store(locale == "zh", Ordering::Relaxed);
    rust_i18n::set_locale(locale);
}

/// Whether the interface is written in Han characters. Answers for the
/// *locale*, not for any particular string — what it feeds is the
/// legibility concession in [`crate::theme::tiny_label`].
pub fn is_cjk() -> bool {
    CJK.load(Ordering::Relaxed)
}

/// Look up a key in the active locale, falling back to English.
pub fn tr(key: &str) -> String {
    t!(key).to_string()
}
