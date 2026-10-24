#!/usr/bin/env bash
#
# gtk4-layer-shell for CI's Ubuntu 24.04 runners, built from source.
#
# Noble ships the GTK 4.14 NSH's GTK backend asks for, but no
# gtk4-layer-shell package (`apt-get install libgtk4-layer-shell-dev`
# failed there: "Unable to locate package"). Debian has carried it since
# trixie, which is where `make linux-check` builds and tests — so this is
# the version trixie carries, 1.0.4, and the build CI runs is against the
# same library the local check runs against. The tarball is checked
# against its digest before a byte of it is built.
#
# Staying on noble rather than moving the jobs into a trixie container is
# deliberate: publish.yml builds the Linux tarballs on these runners, and
# a binary built on trixie needs its glibc (2.41) where noble's is 2.39.
#
# Installs into /usr/local (whose lib and pkgconfig directories noble's
# linker and pkg-config already search) and runs ldconfig, so both the
# build and the test binaries find it.
set -euo pipefail

version=1.0.4
sha256=7fe327dc3740e4b6f5edfd855e23f84b1ac1ec6854b731047b95df7feb46498b

sudo=""
[ "$(id -u)" = 0 ] || sudo=sudo

$sudo apt-get install -y --no-install-recommends \
    ca-certificates curl meson ninja-build libwayland-dev wayland-protocols

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
curl -fsSL -o "$work/src.tar.gz" \
    "https://github.com/wmww/gtk4-layer-shell/archive/refs/tags/v$version.tar.gz"
echo "$sha256  $work/src.tar.gz" | sha256sum -c -
tar -xzf "$work/src.tar.gz" -C "$work"

# Only the library: no GObject introspection, Vala bindings, docs,
# examples or tests, none of which the Rust crate reads.
meson setup "$work/build" "$work/gtk4-layer-shell-$version" \
    --prefix=/usr/local --buildtype=release \
    -Dintrospection=false -Dvapi=false -Ddocs=false \
    -Dexamples=false -Dtests=false -Dsmoke-tests=false
ninja -C "$work/build"
$sudo ninja -C "$work/build" install
$sudo ldconfig
pkg-config --modversion gtk4-layer-shell-0
