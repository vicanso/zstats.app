<!--
Intentionally empty locale directory.

`rust_i18n::i18n!` is pointed here (instead of `assets/locales`) so it embeds
NO translations at compile time. The real `assets/locales/*.toml` are
embedded (compressed) via rust-embed and parsed at runtime; see
`src/i18n_loader.rs`.

Do not add translation files to this folder — edit `assets/locales/*.toml`.
-->
