# zstats

English | [中文](README-zh.md)

A macOS menu-bar system monitor: live CPU in the tray; process monitoring, threshold alerts and disk-space analysis in the panel, with safe reclamation of regenerable caches.

**A menu-bar monitor for macOS.** See what the machine is doing, get told when it matters, take the disk back.

The tray shows live CPU. Click for the panel — it tucks away when you look elsewhere. Collection, alerts and history run in-process on the [zstats](https://crates.io/crates/zstats) engine.

> macOS only · Apple Silicon and Intel · Universal, signed and notarized

<img src="docs/screenshot.png" width="359" alt="zstats panel" />

## Why this one

Most menu-bar monitors either paint pretty numbers or nag you. zstats does both, and then helps you act — disk full: find the 20 GB build cache and trash it the way Finder would (recoverable); memory tight: ask the biggest consumer to quit politely (⌘Q-equivalent).

## Watch

- Live CPU% beside the tray icon
- Overview: P/E cores, memory and compression, kernel memory pressure, disk and network throughput
- Processes ranked by a 60-second average, with a name filter and a one-click full-table scan
- Apps aggregated by process tree — one row for a browser and all its helpers
- Hardware: volumes, hottest sensors first, battery health
- History: what actually burned CPU *today*, ranked by accumulated time, not a spike

## Alert

The headline feature is **per-program thresholds**: on one machine a browser may be allowed 200% CPU while a background daemon should speak up at 30% — every program can carry its own line, edited right on the panel. Built-in templates cover the common cases with sensible defaults, so **alerts are meaningful out of the box, before you configure anything**.

- CPU, memory, disk and memory-pressure thresholds, evaluated by zstats' rule engine
- Native notification banners; snooze an episode for 1 or 3 hours
- Memory-pressure cards list the top consumers and offer a polite quit (⌘Q / SIGTERM — never SIGKILL)

## Reclaim disk

- **Large files, instantly** — Spotlight, ≥500 MB (drops to ≥100 MB when few match)
- **Directory analysis** — a background walk of your home tree (hundreds of thousands of directories in about half a minute) into three rankings: regenerable caches (`CACHEDIR.TAG`), fat directories, and files the index never sees
- Results stream while it walks; click a folder to drill in; pick the analysis root
- Cleanup suggestions: tagged caches plus known tool caches (npm, Cargo, Xcode, …), one click to the Trash, with the owner's own cleanup command shown
- Rules you can replace: drop a `~/.zstats/cleanhints.toml` to override the built-in list

## Safety

The panel acts on the system in exactly two places, both behind a confirm, both reversible:

| Action | What it actually does |
| --- | --- |
| Delete | Finder's move-to-Trash. Never `rm -rf`. |
| Quit | A ⌘Q-equivalent request / SIGTERM. Never SIGKILL. |

Nothing is cleaned or killed automatically. Mail, Messages and other protected data are skipped without a touch. The one-time Desktop / Documents / Downloads prompt on first analysis *is* the analysis.

## Install

Download `zstats.dmg` from [Releases](../../releases) and drag it into Applications.

```bash
make bundle          # or build from source (needs cargo-bundle)
```

Language, theme and panel opacity live in a settings window. The UI is fully bilingual; dark and light modes use native vibrancy.

## Develop

```bash
make dev             # panel stays open
make lint && make test
```

Design notes: [docs/design.md](docs/design.md) · [docs/disk-analysis.md](docs/disk-analysis.md)

Apache-2.0 · [gpui](https://github.com/zed-industries/zed) · [zstats](https://crates.io/crates/zstats)
