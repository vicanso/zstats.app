# Release version — Cargo.toml is the single source of truth.
VERSION := $(shell sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)

.PHONY: dev debug run fmt lint test check release bundle bloat udeps clean version

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
	echo "LSUIElement=true -> $$PLIST"

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
