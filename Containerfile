# The Linux side of the port, built from a macOS machine — `make linux-check`.
# See docs/omarchy-port.md for why this exists and what it can and cannot
# answer (it compiles and tests; it has no compositor, so it cannot show a
# window).
# trixie rather than bookworm: NSH's GTK backend asks for GTK 4.14, and
# bookworm ships 4.8.
FROM docker.io/library/rust:1.98-trixie

# GTK 4 and gtk4-layer-shell are the interface's (NSH's GTK backend and
# the panel's layer surface); everything else builds from crates.
RUN apt-get update && apt-get install -y --no-install-recommends \
        pkg-config libgtk-4-dev libgtk4-layer-shell-dev \
    && rm -rf /var/lib/apt/lists/*

# Linted there as on macOS (`make linux-check` runs clippy); the image
# ships without clippy.
RUN rustup component add clippy
