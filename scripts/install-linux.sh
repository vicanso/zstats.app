#!/usr/bin/env sh
#
# User-level install: binary, icon, launcher entry, autostart.
# No root, nothing outside $HOME.
#
# Linux only, and the name says so — but the runtime guard below stays,
# because a file name cannot stop anyone running this on macOS. There a
# bare binary is not an install at all: `notify.rs` refuses to deliver a
# banner outside a real `.app` bundle, and none of the XDG paths this
# writes to mean anything. macOS installs the signed .app instead.
#
# Three sources, tried in that order, because the same file ships inside
# the release tarball and also stands on its own:
#
#   1. a binary next to this script   — unpacked tarball; no network
#   2. the repo's target/release      — a local build
#   3. the latest release, downloaded — this script on its own
#
# `--uninstall` removes exactly what this put down. `--no-autostart`
# installs everything else. `--tag vX.Y.Z` pins the download.
#
# Not meant to be piped into a shell. It stops a running process and
# writes under ~/.local and ~/.config; that is not something to run
# sight-unseen out of a pipe.

set -eu

APP=zstats
REPO=vicanso/zstats.app
# Both hosts carry the same files under the same names, and their
# download URLs differ only in the root — measured against the live
# mirror, which is why `updater.rs` spells them out instead of asking an
# API for each one. GitHub first because it is the origin; Gitee second
# because for the networks that cannot reach GitHub at all it is the
# only one, and that is the entire reason the mirror exists.
GITHUB_API="https://api.github.com/repos/$REPO"
GITHUB_DL="https://github.com/$REPO/releases/download"
GITEE_API="https://gitee.com/api/v5/repos/$REPO"
GITEE_DL="https://gitee.com/$REPO/releases/download"

BIN_DIR="${XDG_BIN_HOME:-$HOME/.local/bin}"
DESKTOP_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
ICON_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor/512x512/apps"
AUTOSTART_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/autostart"

say() { printf '%s\n' "$*"; }
die() { printf 'install-linux.sh: %s\n' "$*" >&2; exit 1; }

[ "$(uname -s)" = Linux ] || die "Linux only — macOS installs the signed .app from the release page."

here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)

uninstall() {
    for f in "$BIN_DIR/$APP" "$DESKTOP_DIR/$APP.desktop" \
             "$ICON_DIR/$APP.png" "$AUTOSTART_DIR/$APP.desktop"; do
        if [ -e "$f" ]; then
            rm -f "$f"
            say "removed $f"
        fi
    done
    # The generator keeps serving a unit for a file that is gone until it
    # is told otherwise.
    if command -v systemctl >/dev/null 2>&1; then
        systemctl --user daemon-reload 2>/dev/null || true
    fi
    say ""
    say "Left alone on purpose: ~/.zstats (config, alert history, logs)."
    say "Remove it by hand if you want that gone too."
    exit 0
}

if [ "${1:-}" = --uninstall ]; then
    uninstall
fi

want_autostart=1
pin_tag=""
while [ $# -gt 0 ]; do
    case "$1" in
        --no-autostart) want_autostart=0 ;;
        --tag) shift; pin_tag="${1:-}"; [ -n "$pin_tag" ] || die "--tag needs a version" ;;
        *) die "unknown argument $1 (--uninstall, --no-autostart, --tag vX.Y.Z)" ;;
    esac
    shift
done

# --- what to install ------------------------------------------------------

tmpdir=""
cleanup() {
    if [ -n "$tmpdir" ]; then rm -rf "$tmpdir"; fi
}
trap cleanup EXIT INT TERM

case "$(uname -m)" in
    x86_64|amd64)   arch=x86_64 ;;
    aarch64|arm64)  arch=aarch64 ;;
    *) arch="" ;;
esac
tarball="$APP-linux-$arch.tar.gz"

latest_tag() {
    # One `tag_name` at the top level of `releases/latest` on both hosts.
    # Split on commas first so the match cannot run past its own field.
    curl -fsSL --max-time 20 "$1/releases/latest" 2>/dev/null |
        tr ',' '\n' | grep '"tag_name"' | head -1 |
        sed 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/'
}

# Fetch the tarball into a temp dir and verify it. Sets `unpacked` on
# success. Verifying is not optional: the whole reason this app can have
# a mirror at all is that both hosts serve the *same bytes* and
# SHA256SUMS says which — a second address, never a second build. An
# installer that skipped the check would make the mirror a second trust
# assumption.
fetch_release() {
    [ -n "$arch" ] || die "unsupported architecture $(uname -m) — build from source instead"
    command -v curl >/dev/null 2>&1 || die "curl is needed to download a release"
    command -v sha256sum >/dev/null 2>&1 || die "sha256sum is needed to verify the download"

    tmpdir=$(mktemp -d)
    for host in github gitee; do
        if [ "$host" = github ]; then api="$GITHUB_API"; dl="$GITHUB_DL"; else api="$GITEE_API"; dl="$GITEE_DL"; fi

        tag="$pin_tag"
        if [ -z "$tag" ]; then tag=$(latest_tag "$api") || tag=""; fi
        if [ -z "$tag" ]; then
            say "  $host: no release found"
            continue
        fi

        say "  $host: $tag"
        if ! curl -fsSL --max-time 300 -o "$tmpdir/$tarball" "$dl/$tag/$tarball" 2>/dev/null; then
            say "  $host: $tag carries no $tarball"
            continue
        fi
        if ! curl -fsSL --max-time 60 -o "$tmpdir/SHA256SUMS" "$dl/$tag/SHA256SUMS" 2>/dev/null; then
            say "  $host: $tag has no SHA256SUMS — refusing to install unverified bytes"
            continue
        fi
        if ! (cd "$tmpdir" && grep -F " $tarball" SHA256SUMS | sha256sum -c - >/dev/null 2>&1); then
            die "checksum mismatch on $tarball from $host — not installing"
        fi
        say "  $host: $tarball verified against SHA256SUMS"

        (cd "$tmpdir" && tar -xzf "$tarball")
        unpacked="$tmpdir/$APP-linux-$arch"
        [ -x "$unpacked/$APP" ] || die "the tarball has no $APP binary in it"
        return 0
    done
    die "no Linux release available from either host (built one locally? run 'cargo build --release' first)"
}

if [ -x "$here/$APP" ]; then
    binary="$here/$APP"
    icon="$here/$APP.png"
elif [ -x "$here/../target/release/$APP" ]; then
    binary="$here/../target/release/$APP"
    icon="$here/../assets/$APP-icon.png"
else
    say "no local binary — fetching the latest release for $arch"
    fetch_release
    binary="$unpacked/$APP"
    icon="$unpacked/$APP.png"
fi
[ -f "$icon" ] || icon=""

# --- a running instance would keep serving the old binary -----------------
#
# The panel is single-instance: it holds a socket in $XDG_RUNTIME_DIR, and
# a launch that finds that socket hands its command over and exits. So a
# fresh binary on disk changes nothing at all until the old process is
# gone — the panel that opens is still the old code. Stop it here rather
# than leave that to be discovered.
if pgrep -x "$APP" >/dev/null 2>&1; then
    say "stopping the running $APP (it would keep serving the old binary)"
    pkill -x "$APP" || true
    # Give it a moment to unlink its socket before the new one binds.
    sleep 1
fi

# --- install --------------------------------------------------------------

mkdir -p "$BIN_DIR"
install -m755 "$binary" "$BIN_DIR/$APP"
say "installed $BIN_DIR/$APP"

if [ -n "$icon" ]; then
    mkdir -p "$ICON_DIR"
    install -m644 "$icon" "$ICON_DIR/$APP.png"
    say "installed $ICON_DIR/$APP.png"
fi

# --- shared libraries, the one thing that really is per-distribution ------
#
# Everything above is XDG and identical on every distribution. Library
# *package names* are not, and this binary needs some: xkbcommon, xcb and
# fontconfig/freetype are linked at build time, Wayland and Vulkan are
# dlopened at run time. Without them the first launch dies in the dynamic
# linker, which says nothing a user can act on.
#
# Reported, never installed. A user-level installer that starts calling a
# package manager needs root it was designed not to need, and a guess at
# the wrong manager is worse than a sentence the reader can act on
# themselves.
distro_packages() {
    id=""
    like=""
    if [ -r /etc/os-release ]; then
        id=$(sed -n 's/^ID=//p' /etc/os-release | tr -d '"' | head -1)
        like=$(sed -n 's/^ID_LIKE=//p' /etc/os-release | tr -d '"' | head -1)
    fi
    case " $id $like " in
        *" arch "*|*" archlinux "*)
            echo "sudo pacman -S --needed libxkbcommon libxkbcommon-x11 libxcb fontconfig vulkan-icd-loader" ;;
        *" debian "*|*" ubuntu "*)
            echo "sudo apt install libxkbcommon0 libxkbcommon-x11-0 libxcb1 libfontconfig1 libvulkan1" ;;
        *" fedora "*|*" rhel "*|*" centos "*)
            echo "sudo dnf install libxkbcommon libxkbcommon-x11 libxcb fontconfig vulkan-loader" ;;
        *" suse "*|*" opensuse "*)
            echo "sudo zypper install libxkbcommon0 libxkbcommon-x11-0 libxcb1 fontconfig libvulkan1" ;;
        *)
            echo "" ;;
    esac
}

report_missing_libs() {
    command -v ldd >/dev/null 2>&1 || return 0
    missing=$(ldd "$1" 2>/dev/null | awk '/not found/ { print $1 }' | sort -u)
    [ -n "$missing" ] || return 0
    say ""
    say "warning: $APP cannot start yet — these shared libraries are missing:"
    echo "$missing" | sed 's/^/  /'
    pkgs=$(distro_packages)
    if [ -n "$pkgs" ]; then
        say "Install them with:"
        say "  $pkgs"
    else
        say "Install your distribution's xkbcommon, xkbcommon-x11, xcb,"
        say "fontconfig and Vulkan loader packages."
    fi
    # Not exhaustive on purpose: Wayland and Vulkan are dlopened, so a
    # missing one of those never appears here and shows up as a failure
    # to start instead. They are in the package lists above for that
    # reason.
}

report_missing_libs "$BIN_DIR/$APP"

# Two entries, same content, different jobs: one puts it in the launcher,
# one starts it at login. `StartupNotify=false` in both — the panel is a
# layer-shell surface and never maps an ordinary toplevel, so a startup
# notification has nothing to resolve against and the cursor spins until
# it times out.
write_entry() {
    cat > "$1" <<EOF
[Desktop Entry]
Type=Application
Name=$APP
Comment=Menu-bar metrics panel
Exec=$BIN_DIR/$APP
Icon=$APP
Terminal=false
StartupNotify=false
Categories=Utility;System;Monitor;
EOF
}

mkdir -p "$DESKTOP_DIR"
write_entry "$DESKTOP_DIR/$APP.desktop"
say "installed $DESKTOP_DIR/$APP.desktop"
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$DESKTOP_DIR" 2>/dev/null || true
fi

# --- autostart, only where something actually reads it ---------------------
#
# A compositor is not a session manager. Hyprland, Sway, river and niri do
# not read ~/.config/autostart themselves; on a systemd distro that
# directory is served by systemd-xdg-autostart-generator, and in a full
# desktop it is served by that desktop's own session manager. Where
# neither is true, dropping the file in would look like an install and do
# nothing — so this prints the line to add instead.
autostart_reader() {
    if command -v systemctl >/dev/null 2>&1 &&
       systemctl --user is-active xdg-desktop-autostart.target >/dev/null 2>&1; then
        echo "systemd (xdg-desktop-autostart.target)"
        return 0
    fi
    case ":${XDG_CURRENT_DESKTOP:-}:" in
        *:GNOME:*|*:KDE:*|*:XFCE:*|*:X-Cinnamon:*|*:MATE:*|*:LXQt:*|*:Budgie:*|*:Pantheon:*|*:Deepin:*)
            echo "${XDG_CURRENT_DESKTOP} session manager"
            return 0 ;;
    esac
    return 1
}

compositor_hint() {
    if [ -n "${HYPRLAND_INSTANCE_SIGNATURE:-}" ]; then
        say "  hyprland.conf:  exec-once = $BIN_DIR/$APP"
    elif [ -n "${SWAYSOCK:-}" ]; then
        say "  sway config:    exec $BIN_DIR/$APP"
    else
        say "  your compositor's startup config:  $BIN_DIR/$APP"
    fi
}

if [ "$want_autostart" = 0 ]; then
    say "skipped autostart (--no-autostart)"
elif reader=$(autostart_reader); then
    mkdir -p "$AUTOSTART_DIR"
    write_entry "$AUTOSTART_DIR/$APP.desktop"
    say "installed $AUTOSTART_DIR/$APP.desktop — read by $reader"
    # The generator only sees files that existed when it last ran.
    if command -v systemctl >/dev/null 2>&1; then
        systemctl --user daemon-reload 2>/dev/null || true
    fi
else
    say ""
    say "No XDG autostart in this session, so nothing was installed for it."
    say "Add this line instead:"
    compositor_hint
fi

# --- afterwards -----------------------------------------------------------

case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *) say ""; say "note: $BIN_DIR is not on PATH; start it with $BIN_DIR/$APP" ;;
esac

say ""
say "Start it now:  $BIN_DIR/$APP"
say "Toggle it:     $BIN_DIR/$APP --toggle   (bind this to a key)"
