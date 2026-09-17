# Release version — Cargo.toml is the single source of truth.
VERSION := $(shell sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)

.PHONY: dev debug run fmt lint test check release bundle bloat udeps clean version linux-check linux-clean

# --- develop ---------------------------------------------------------------

# Debug build + launch. `cargo run` keeps the console attached, so tracing /
# panics land in the terminal (release builds detach on Windows).
dev:
	bacon run

debug:
	RUST_LOG=debug $(MAKE) dev

run: dev

# --- quality ---------------------------------------------------------------

fmt:
	cargo fmt

lint:
	cargo clippy --all-targets --all-features -- --deny=warnings

# Type-check only — far faster than a full build when iterating on code.
check:
	cargo check --all-targets

test:
	cargo test --workspace

# --- release ---------------------------------------------------------------

release:
	cargo build --release

# Desktop app bundle (.app / .deb / .msi). Requires `cargo install cargo-bundle`.
#
# The LSUIElement step is what keeps the Dock icon from flashing on launch.
# `hide_dock_icon()` only runs ~50ms in — after gpui has built NSApplication,
# installed its delegate and started the runloop — and the icon is in the Dock
# for all of it. LSUIElement makes LaunchServices skip creating it in the first
# place, so all that's left is gpui's own setActivationPolicy(Regular), which
# our callback undoes microseconds later in the same call stack.
bundle:
	cargo bundle --release
	@TARGET=$$(cargo metadata --format-version 1 --no-deps \
		| sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p'); \
	APP=$$(find "$$TARGET/release/bundle/osx" -maxdepth 1 -name '*.app' 2>/dev/null | head -1); \
	if [ -z "$$APP" ]; then echo "no .app bundle found (non-macOS target?)"; exit 0; fi; \
	PLIST="$$APP/Contents/Info.plist"; \
	/usr/libexec/PlistBuddy -c "Delete :LSUIElement" "$$PLIST" >/dev/null 2>&1 || true; \
	/usr/libexec/PlistBuddy -c "Add :LSUIElement bool true" "$$PLIST"; \
	echo "LSUIElement=true -> $$PLIST"; \
	codesign -s - --force --deep "$$APP"; \
	echo "ad-hoc signed as bundle identifier -> $$APP"; \
	/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -u "$$APP" >/dev/null 2>&1 || true; \
	echo "unregistered from LaunchServices -> $$APP"
# The codesign matters even for a local build: the linker's automatic ad-hoc
# signature carries a random identifier (zstats-<hex>), and
# UNUserNotificationCenter refuses authorization to it — banners silently
# absent. Re-signing reads the identifier from Info.plist, which is the
# identity notifications are granted to (measured both ways; docs/design.md
# 系统通知). The release pipeline's real signature replaces this one.
# The unregister matters: the build output carries the same bundle id as the
# installed /Applications/zstats.app, Spotlight registers any .app it indexes,
# and two live registrations of one id make notification attribution sway —
# banners silently lost (measured; see docs/design.md 系统通知). Unregistering
# here keeps the installed copy the only claimant; Spotlight may quietly
# re-add this one later, which is why it runs on every bundle rather than once.

# --- linux port ------------------------------------------------------------

# Compile and test the Linux side from this macOS machine, in a container.
# See docs/omarchy-port.md; the short version is that it answers "does it
# build and do the tests pass", and nothing about how the panel looks —
# there is no compositor in there.
#
# arm64 only: Apple's `container` runs arm64 VMs with no emulation, so
# x86_64 is CI's job (`.github/workflows/test.yml`).
#
# Three things here were learned the hard way. crates.io times out inside
# that VM, so dependencies are vendored on the host and the build runs
# `--offline` against them. `naga` needs more than the default memory or
# the OOM killer takes it mid-compile. And the work directory lives outside
# the repo so a Linux target tree never collides with the macOS one.
LINUX_WORK ?= $(HOME)/.cache/zstats-linux

linux-check:
	@command -v container >/dev/null || { echo "needs Apple's container CLI (or swap in docker/podman)"; exit 1; }
	@container image list | grep -q '^zstats-linux' || container build -t zstats-linux -f Containerfile .
	@[ -d "$(LINUX_WORK)/vendor" ] || cargo vendor --locked "$(LINUX_WORK)/vendor" >/dev/null
	@mkdir -p "$(LINUX_WORK)/target" "$(LINUX_WORK)/cargo-home"
	@printf '[source.crates-io]\nreplace-with = "vendored-sources"\n\n[source.vendored-sources]\ndirectory = "/vendor"\n' > "$(LINUX_WORK)/cargo-home/config.toml"
	container run --rm -m 12g -c 6 \
		-v "$(CURDIR)":/src \
		-v "$(LINUX_WORK)/target":/target \
		-v "$(LINUX_WORK)/vendor":/vendor \
		-v "$(LINUX_WORK)/cargo-home":/cargo-home \
		-w /src -e CARGO_TARGET_DIR=/target -e CARGO_HOME=/cargo-home \
		zstats-linux \
		bash -c "cargo build --all-targets --offline && cargo test --offline"

# Drop the container work tree (vendored crates and the Linux target dir).
linux-clean:
	rm -rf "$(LINUX_WORK)"

# Where the release binary's size goes, by crate.
bloat:
	cargo bloat --release --crates --bin zstats

# Unused dependencies (nightly-only).
udeps:
	cargo +nightly udeps

clean:
	cargo clean

version:
	@echo $(VERSION)
	git cliff --unreleased --tag v$(VERSION) --prepend CHANGELOG.md

# Regenerate CHANGELOG.md from conventional commits (git-cliff, cliff.toml).
# Run it in the version-bump commit, before tagging — CI does not write
# back to main, so the file is only ever updated here.
changelog:
	git cliff -o CHANGELOG.md
