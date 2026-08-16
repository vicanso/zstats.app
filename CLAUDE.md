# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

macOS menu-bar metrics panel: gpui + gpui-component for the UI, the `zstats` crate (`frontend` feature, no tokio) for collection and alerting.

`docs/design.md` is the design record (README.md is the user-facing GitHub page, README-zh.md its Chinese twin — keep the pair in sync when editing either) — it explains *why* nearly every non-obvious decision here is the way it is (window reuse, Dock suppression, tray rasterisation, vibrancy layering, the multi-display workarounds). Read the relevant section before changing any of that machinery; the odd-looking code is usually load-bearing.

## Commands

```bash
make dev      # cargo run (debug: opens the panel immediately, no auto-hide)
make debug    # RUST_LOG=debug cargo run
make check    # cargo check --all-targets
make lint     # cargo clippy --all-targets --all-features -- --deny=warnings
make test     # cargo test --workspace
make release  # cargo build --release
make bundle   # .app + writes LSUIElement into Info.plist (needs cargo-bundle)
```

Single test: `cargo test <name>` (e.g. `cargo test anchored_origin`) — tests are inline `#[cfg(test)] mod tests` at the bottom of each module, not a `tests/` directory.

`ZSTATS_DEBUG_POSITION=1` prints the whole tray-anchor → window-origin conversion chain; use it for multi-display placement bugs.

## Platform reality

macOS is the only supported target. Other platforms compile but the `#[cfg(not(target_os = "macos"))]` branches (window destroy-instead-of-hide, `cx.displays()` lookup, no vibrancy) have never been run; Linux has no tray at all (`tray.rs` is entirely cfg'd out, and `tray-icon` is declared only under `cfg(not(target_os = "linux"))`). Don't treat those branches as tested paths.

## Architecture

**One root view, one global store.** `views/` modules are plain functions that read `ZStatsGlobalStore` (a `Global` wrapper around `Entity<ZStatsAppState>`) and return elements — they are *not* gpui views. The whole panel repaints per tick. Anything that must survive hide → re-show goes in `state.rs`, including per-tab `ScrollHandle`s: gpui discards element state for anything not painted, and only the active tab is painted. The deliberate exception is query-like state — the name filter and the one-shot full listings — which both hide paths reset via `reset_transient_views`: a panel reopened hours later with yesterday's query looks broken, not remembered.

**Collection is a resident background thread** (`metrics.rs`) running `zstats::Monitor::tick()`, handing `Tick`s to the main thread over a `smol::channel`. It must outlive the window — `Monitor` holds the previous-sample baselines that every rate is diffed against. Cadence is adaptive (config `interval` when visible or busy, 5s idle) and waits with `recv_timeout` so opening the panel samples immediately. First-sample rate metrics are legitimately `None` → UI shows `—`.

**Config is shared with the zstats CLI** via `zstats::settings::default_dir()` (`~/.zstats`). The Config view (its own standard window since the footer gained the gear — a settings session must not die to the popover's auto-hide; `open_settings_window` in main.rs, reuse-or-rebuild) writes through `zstats::settings::apply_add` (same keys as the CLI's `-add`) then either `metrics::request_reload()` → `Monitor::reload_settings()` for `[alerts]`, or `metrics::request_rebuild()` for `[collector]` / `[daemon]` — those are baked into `LocalCollector` at construction, so the first sample after a rebuild legitimately reads `—`. Running `zstats serve` alongside this app double-collects; there is no detection for it.

**UI preferences (language / theme) live in `~/.zstats/app.toml`** (`prefs.rs`), edited from the Config window's interface card. Deliberately not in config.toml: `zstats::settings::save` serialises only the sections the CLI models, so any extra key there would be dropped on the next `apply_add` round-trip. A forced theme also pins `NSApp.appearance` (`apply_ns_appearance` in `main.rs`) so the vibrancy material follows it; a language switch rebuilds the tray and app menus, which snapshot their translated titles at build time.

**zstats owns both the alerts and the numbers. Treat this as a hard rule.**

- **Alerts.** Every `AlertEvent` is produced by zstats' rule engine from the thresholds in `~/.zstats/config.toml`. This app evaluates no alert condition of its own. `state.rs` only merges incoming events into episodes keyed by `(subject, kind)` and caps the list at 20; the Alerts tab edits thresholds through `zstats::settings::apply_add`, so the panel and the CLI can never disagree about when something fires. A new alert belongs in zstats' rule engine, not here. The banner snooze (`state.rs` `snooze_banners`) does not bend this rule: it filters *delivery* of banners per episode for a bounded time — the engine keeps evaluating, the list keeps recording, config.toml is untouched, and nothing persists across restart.
- **Metric values.** Every raw figure the UI renders comes off the `Tick` — `snapshot.*`, `process_stats` (the rolling averages), and `records::read_range` for History. Do not recompute a number zstats already reports, even when the inputs are sitting right there: `views/disk.rs` deliberately renders `DiskSnapshot::used_percent` instead of dividing used by total, because a locally derived figure could only ever disagree with what the CLI paints and what the disk alert fires on. Views format and colour; they do not derive.

The one carve-out is `watch.rs`, and it is about *what question is asked*, not about a second source of numbers — it still reads zstats' own fields (`cpu_time_ms`, network snapshots). Its three observers — sustained low-grade CPU (integrated from `cpu_time_ms`, bar = `alert-cpu / 3`), abnormal processes, interface activity — exist because zstats structurally cannot see them: sustained load never crosses a threshold by definition, zombies are ranked out of the process table before the UI sees it, and the kernel's cumulative network counters cannot say *when* bytes moved. None of them is an `AlertEvent`; only sustained load reaches the notification centre, deliberately as a silent banner (`notify::post_sustained`). `procscan.rs` is the sole place that queries the OS directly (`sysctl(KERN_PROC_ALL)`), and it reports process *state*, not metrics. `watch.rs` touches no gpui types precisely so it can be tested against hand-built sample sequences. `diskscan.rs` (the Hardware tab's directory analyser — see docs/disk-analysis.md) is the same class of one-shot panel-owned OS query, with one deliberate deviation: its minutes-long walk **survives panel hide** (only the explicit cancel stops it), because the panel auto-hides on any focus loss and a hide-reset would mean no analysis ever completes. The run lives on its own thread and reports progress and completion exclusively over a channel, run-id-guarded so a superseded run's events land nowhere.

The app *acts* on the system in exactly two places, both click-plus-confirm, both delivering refusable or recoverable requests. `bigfiles::trash` is Finder's own move-to-Trash (`NSFileManager.trashItemAtURL`, never a direct unlink) behind the Hardware tab's large-file rows and the analyser's regenerable-cache rows — the latter only for signature-checked `CACHEDIR.TAG` trees, per-row or "the N listed" in bulk, never for heuristic or plain directories. `terminate.rs` is the other, and it stays strictly on the "how" side of the rule: the quit button on memory alert cards consumes zstats' `AlertEvent` (deciding *when* something is over the line stays in the rule engine) and delivers a refusable request — `NSRunningApplication.terminate()` (⌘Q-equivalent) for applications, SIGTERM for bare processes, never SIGKILL. Only ever behind a user click plus the `confirm.rs` sheet; nothing is evicted automatically, and the button is not rendered for pids the user cannot signal (`kill(pid, 0)` gate).

The `HOT_*` / `FULL_PERCENT` / `CORE_HOT` constants in `views/` are the only thresholds allowed to live in this repo, and they change colour only — they never produce an alert or a notification.

**Window model.** The window is created once and shown/hidden/moved through AppKit directly (`window_ext.rs`), bypassing gpui's create-or-destroy model, which leaks ~1 MB per cycle here. Repaint is gated on `CollectorPace::is_visible()` — the window is moved off screen, so gpui would otherwise keep rendering it.

**Startup order in `main()` is a constraint, not a style choice**: `dock::suppress_regular_policy()` before the run loop; `gpui_component::init` before any component; `prefs::load()` before the theme and locale it feeds; `apply_appearance` before the first frame; `i18n::init()` before the first `t!`; `set_quit_mode(QuitMode::Explicit)` before anything can close a window.

## Conventions

- **Module paths**: cross-module *functions* are called with one level of qualification — `use crate::diskscan;` at the top, `diskscan::save_cache(...)` at the call site. Never inline `crate::diskscan::save_cache(...)` in a body, and never import bare function names (the module prefix is what tells a reader in `state.rs` whose side effect this is). *Types* are imported directly (`use crate::diskscan::{self, ScanResult}` — fold the module in via `self` when both are needed). Crate-root items (`crate::APP_NAME`, `crate::open_settings_window`) stay fully qualified: there is no module level to keep.
- **Wide parameter lists become structs.** When a function trips clippy's `too_many_arguments`, group the parameters into a named-field struct (`AnalysisRow`, `Aggregates`) instead of adding an `#[allow]` — the lint is right about the call sites, and named fields are what fix them.
- **Comments explain why, not what.** Module-level `//!` docs carry the reasoning, and most constants have a doc comment justifying their value (often with measured numbers). Match that — a new tunable without its rationale is out of place here.
- **Threshold colouring goes through `theme::fill_for()` / `theme::text_for()`**: neutral `ink` until over the line, brand `accent` after. Don't hand-pick colours for this.
- **Custom SVGs go through `assets::CustomIconName`**, never a raw `Icon::empty().path("icons/…")`: the filename then lives in one place, `assets.rs` embeds an explicit allowlist (`#[include]`), and a test asserts every name resolves to an embedded file. Anything gpui-component's `IconName` already has should use that instead of shipping another SVG.
- **Clickable things need a visible affordance** — hover fill, a border that lifts, or real button chrome. macOS keeps the arrow over every native control and reserves the hand cursor for links, so `cursor_pointer` appears exactly once in the app: the footer's GitHub button, the only control that opens something outside it. Don't put it on buttons, rows or tabs; see the affordance rule in `views/mod.rs`.
- **i18n**: `i18n!` is pointed at the empty `locales_stub/` on purpose; real strings live in `assets/locales/{en,zh}.toml`, embedded compressed and parsed lazily by `i18n_loader.rs`. Add every key to *both* files — a test enforces parity, because rust-i18n silently falls back and a missing key just shows one stray English string.
- **Cross-file invariants have tests guarding them**: the notification bundle id must equal `[package.metadata.bundle] identifier` in `Cargo.toml`, and the tray icon rasterisation is checked for coverage (a parse failure yields a fully transparent bitmap and no error).
- **Dependency pinning**: `gpui`/`gpui_platform`/`gpui_macros` are git deps *without* `rev=`, matching gpui-component's own source form — Cargo treats `?rev=X` and no-rev as different package identities even at the same commit. Only gpui-component's rev is pinned. After any bump, confirm one gpui with `cargo tree -i gpui` and commit `Cargo.lock`.
- Measure CPU with two cputime samples over wall clock, never `ps -o %cpu` (a decayed average polluted by startup).
