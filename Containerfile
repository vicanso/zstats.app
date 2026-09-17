# The Linux side of the port, built from a macOS machine — `make linux-check`.
# See docs/omarchy-port.md for why this exists and what it can and cannot
# answer (it compiles and tests; it has no compositor, so it cannot show a
# window).
FROM docker.io/library/rust:1.98-bookworm

# xkbcommon links at build time (gpui turns the crate's default features
# off, so there is no dlopen shim), and gpui's x11 backend links
# xkbcommon-x11 and xcb alongside it — a missing one of those shows up at
# the *link* step, long after everything has compiled. fontconfig and
# freetype back font-kit. Wayland and Vulkan are dlopened at runtime, so
# they are not needed to build.
RUN apt-get update && apt-get install -y --no-install-recommends \
        pkg-config libxkbcommon-dev libxkbcommon-x11-dev libxcb1-dev \
        libfontconfig1-dev \
    && rm -rf /var/lib/apt/lists/*
