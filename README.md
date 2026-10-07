# zstats

English | [中文](README-zh.md)

A macOS menu-bar system monitor built around per-app rules: live CPU in the tray; process monitoring, threshold alerts each program can carry its own line for, and disk-space analysis with safe reclamation of regenerable caches.

**A menu-bar monitor that watches your machine your way.** Different apps deserve different rules — set them per app, get told when *your* lines are crossed, take the disk back.

The tray shows live CPU. Click for the panel — it tucks away when you look elsewhere. Collection, alerts and history run in-process on the [zstats](https://crates.io/crates/zstats) engine.

In the menu bar, panel tucked away, the CPU cost stays low. One measured run of the installed app: up for 14 hours 37 minutes, 5 minutes 38 seconds of CPU time, **0.64% of one core**.

> macOS · Apple Silicon and Intel · Universal, signed and notarized — and a Linux preview, see Install

<p align="center">
  <video src="https://github.com/user-attachments/assets/42259bb0-acb7-4675-9c3c-6ad791cca455" autoplay loop muted playsinline width="100%"></video>
</p>

## Why this one

Most menu-bar monitors paint pretty numbers, nag you, or both — and they treat every program the same. zstats starts from the opposite premise: **monitoring is personal**. The browser you live in all day is allowed 200% CPU; the daemon you forgot about should speak up at 30% — so every threshold here can be set per app, right on the panel, with sensible built-in templates covering the common cases before you configure anything. It still paints the numbers, still tells you when it matters, and then helps you act — disk full: find the 20 GB build cache and trash it the way Finder would (recoverable); memory tight: ask the biggest consumer to quit politely (⌘Q-equivalent).

## Watch

- Live CPU% beside the tray icon — and while a memory alert you have not dismissed is on the Alerts tab (a process, an app, or kernel pressure), the item turns into a memory stick with the memory still available instead; a disk-full alert turns it into a disk with the space still free. Or pin it to CPU or memory for good, or keep both side by side
- ⌘1–7 switch tabs, and the panel reopens on the one you left; pin it (the footer pin, or ⌘P) to keep it up beside another window instead of hiding on focus loss
- Keep the Mac awake for a set time, 30 minutes to 8 hours, picked from the cup in the panel's footer: the screen stays on, nothing locks from sitting idle and the system does not idle-sleep, so a long job finishes and you can still read it. One power assertion — the one `caffeinate -d -t` takes — with its end time on the assertion, so macOS drops it on the minute, on battery too. The cup stays lit until then and says when; Off in its menu gives the Mac back its own sleep settings at once. A lock or sleep you ask for, and the lid, still win
- Overview: P/E cores, uptime and live power draw, memory and compression, kernel memory pressure, disk and network throughput
- Apps aggregated by process tree — one row for a browser and all its helpers
- Processes ranked by a 60-second average, by memory, or by disk IO, with a name filter and a one-click full-table scan
- Hardware: volumes, each physical drive's IOPS, latency and queue depth, the GPU, hottest sensors first, battery health — the drive and GPU reads run only while this tab is open
- Network: which program is listening on which port, the ones open to other machines first, searchable by program, pid, port or address — read only while the tab is open, and in the installed app (macOS shows a development build only its own sockets); then per-interface rates, packets per second on hover, and error rates only while an interface is actually erroring
- History: what actually burned CPU *today*, ranked by accumulated time, not a spike; click a row to open that app or process

## Alert

The headline is **a base threshold combined with per-program ones**: one global line as the floor, and any program can carry its own — on one machine a browser may be allowed 200% CPU while a background daemon should speak up at 30%. A program's own line wins where set, the base line covers the rest, edited right on the panel. Built-in templates cover the common cases with sensible defaults, so **alerts are meaningful out of the box, before you configure anything**.

- CPU, memory, disk and memory-pressure thresholds, evaluated by zstats' rule engine
- **Two granularities**: rules watch the single **process** and, separately, the whole **application** — its process tree summed. A runaway helper trips the process line; a browser quietly holding 4 GB across 37 helpers trips the application line, though no single member ever crosses one. Each level carries its own base threshold and its own per-name overrides
- **Slow burns get named too**: a process holding CPU for hours without ever crossing a line (25% for an hour, say) is called out by the sustained-load watcher — delivered as a silent banner, never a nag. How long counts as "sustained", and how far under the alert line the bar sits, are yours to set
- Native notification banners; snooze an episode for 1 or 3 hours
- Every card jumps to its subject: the process, the app tree, or the disk tools for a full volume
- Memory-pressure cards list the top consumers and offer a polite quit (⌘Q / SIGTERM — never SIGKILL)

## Reclaim disk

- **Large files, instantly** — straight from macOS's own Spotlight index, no disk walk: ≥500 MB (drops to ≥100 MB when few match)
- **Directory analysis** — a background walk of your home tree (hundreds of thousands of directories in about half a minute) into three rankings: regenerable caches (`CACHEDIR.TAG`), fat directories, and files the index never sees
- **Duplicate files** — identical content, any file type: sizes first, then the first 64 KB, and only files still alike are read whole, so a search of your home folder (files of 1 MB and up) takes seconds; pick a folder and it is searched at once, every file in it. Hard links and APFS clones are recognised, so "can be freed" never counts space that would not come back; tool-owned trees (`node_modules`, Go's module cache, a project's `vendor`) and app packages are left alone. Move single copies to the Trash — never the last one
- All three live in one window, a tab each, wide enough to read a path in full; results stream while the walk runs, click a folder to drill in, pick the analysis root
- Cleanup suggestions **speak only by rule**: a directory either carries a signature-checked `CACHEDIR.TAG` (the owner's own declaration that it is regenerable) or matches a cache list compiled from **each tool's official documentation** (npm, Cargo, Xcode, …) — name-based guesses are labelled, never suggested. Every suggestion says which of the two it rests on, and flags a cache whose app is still running (moving it out from under the app frees nothing until the app quits). One click to the Trash, with the owner's own cleanup command shown; the bulk action lists every item with its size so you can untick any, and the window reminds you the space comes back only when the Trash is emptied
- **What grew this week**: once you have analysed Home, a daily check re-walks it in the background — only on power, while the Mac is quiet and the panel is closed, on one throttled thread — and the window lists the folders that grew most over the past week. A folder that grows by 5 GB or more gets one silent notification. A switch in Settings → Interface, or the Daily check chip on the analysis card, turns it off
- Rules you can replace: drop a `~/.zstats/cleanhints-macos.toml` to override the built-in list

## Safety

The panel acts on the system in exactly two places, both behind a confirm, both reversible:

| Action | What it actually does |
| --- | --- |
| Delete | Finder's move-to-Trash. Never `rm -rf`. |
| Quit | A ⌘Q-equivalent request / SIGTERM. Never SIGKILL. |

The optional keep-awake hold is the only other thing the panel asks of the system: one power assertion with an end time, visible in the footer while it is held, released when its time is up, when you end it, or when you quit.

Nothing is cleaned or killed automatically. Mail, Messages and other protected data are skipped without a touch. The one-time Desktop / Documents / Downloads prompt on first analysis *is* the analysis.

## Install

Download `zstats.dmg` from [Releases](../../releases) and drag it into Applications.

Behind the Great Firewall, the same files are mirrored to [Gitee](https://gitee.com/vicanso/zstats.app/releases) on every release: the three DMGs and the `SHA256SUMS` they are listed in. The in-app updater falls back to that mirror on its own when GitHub cannot be reached, and verifies the download against the same checksum either way.

```bash
make bundle          # or build from source (needs cargo-bundle)
```

Language, theme, the tray's face, panel opacity and the sustained-load knobs live in a settings window. The UI is fully bilingual; dark and light modes use native vibrancy.

### Linux (preview)

Wayland only: the tray is a StatusNotifier item, the panel a layer-shell surface anchored top-right by the compositor. Developed and checked against Omarchy (Hyprland); other compositors and desktops are untested. Every release carries `zstats-linux-x86_64.tar.gz` and `zstats-linux-aarch64.tar.gz`, listed in the same `SHA256SUMS` and mirrored to Gitee.

The quickest route is the install script on its own: it downloads the latest release for this machine's architecture — GitHub first, then the Gitee mirror — and refuses to install anything `SHA256SUMS` does not vouch for.

```bash
curl -fsSLO https://raw.githubusercontent.com/vicanso/zstats.app/main/scripts/install-linux.sh
sh install-linux.sh              # --tag vX.Y.Z pins a release · --no-autostart · --uninstall
```

Download it and run it rather than piping it into `sh`: it stops a running zstats and writes under `~/.local` and `~/.config`, which is worth a read first. Where `raw.githubusercontent.com` is out of reach, take the tarball from the Gitee mirror instead — the script inside installs the binary next to it and needs no network:

```bash
tar -xzf zstats-linux-x86_64.tar.gz
sh zstats-linux-x86_64/install-linux.sh
```

Either way that puts the binary in `~/.local/bin`, adds a launcher entry, and enables launch-at-login where the session actually reads `~/.config/autostart` (it checks, and prints the `exec-once` line to add instead when nothing does); `--uninstall` removes exactly what it added. Then, for Hyprland:

```conf
bind = SUPER, M, exec, ~/.local/bin/zstats --toggle
layerrule = blur, zstats
```

On Omarchy, Settings → Interface → Theme offers **Omarchy**: the panel takes the desktop's current theme — its colours, accent and light or dark — and follows a theme switch the next time it opens.

What the Linux build does not do: no memory-pressure or P/E-core figures (the kernel reports neither the way macOS does), no Time Machine or purgeable-space lines, and the tray wears a glyph with no number beside it — StatusNotifier has no text. The in-app updater installs the tarball in place and restarts; a binary owned by a package manager is left to that package manager.

## Develop

```bash
make dev             # panel stays open
make lint && make test
```

Design notes: [docs/design.md](docs/design.md) · [docs/disk-analysis.md](docs/disk-analysis.md)

Apache-2.0 · [gpui](https://github.com/zed-industries/zed) · [zstats](https://crates.io/crates/zstats)
