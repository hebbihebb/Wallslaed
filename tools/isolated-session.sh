#!/usr/bin/env bash
# Run a command in a throwaway desktop environment: its own HOME, XDG folders,
# runtime folder and D-Bus session bus, so nothing it does (gsettings, dconf, a
# nested gnome-shell, the daemon, the player) reaches the real desktop.
#
#   tools/isolated-session.sh [--install] <scratch-dir> <command> [args...]
#
# --install first installs the current build into <scratch-dir>/home, like
# install.sh does for the real home (run `cargo build --release` before).
#
# Inside, a headless shell starts with:
#   gnome-shell --headless --virtual-monitor 1920x1080 --wayland --no-x11 &
#   gdbus wait --session --timeout 40 org.gnome.Shell
set -euo pipefail
cd "$(dirname "$0")/.."

install=false
if [[ "${1:-}" == "--install" ]]; then
    install=true
    shift
fi
if [[ $# -lt 2 ]]; then
    echo "usage: $0 [--install] <scratch-dir> <command> [args...]" >&2
    exit 2
fi
scratch=$(realpath -m "$1")
shift
home="$scratch/home"

case "$home" in
    "$HOME" | "$HOME"/.* | "$HOME/.config"*) echo "refusing: scratch home must not be inside the real home's config" >&2; exit 2 ;;
esac
mkdir -p "$home"

if $install; then
    uuid=wallslaed@hebbihebb
    ext="$home/.local/share/gnome-shell/extensions/$uuid"
    schemas="$home/.local/share/glib-2.0/schemas"
    install -Dm755 -t "$home/.local/bin" target/release/wallslaed target/release/wallslaed-player
    install -Dm644 -t "$schemas" data/io.github.hebbihebb.Wallslaed.gschema.xml
    glib-compile-schemas --strict "$schemas"
    command rm -rf "$ext"
    install -Dm644 -t "$ext" extension/$uuid/*.js extension/$uuid/metadata.json
    install -Dm644 -t "$ext/schemas" data/io.github.hebbihebb.Wallslaed.gschema.xml
    glib-compile-schemas "$ext/schemas"
fi

# Wayland socket paths must fit in 108 bytes, so the runtime folder is short.
runtime=$(mktemp -d "/run/user/$(id -u)/wsl-XXXXXX")
trap 'command rm -rf "$runtime"' EXIT

before=$(dconf dump / 2>/dev/null || true)
# The environment must be set *before* dbus-run-session. Services the bus
# starts on demand (dconf-service, goa, evolution, portals) inherit the bus's
# environment, not the command's; set it later and their writes land in the
# real home.
status=0
env -u WAYLAND_DISPLAY -u DISPLAY -u XDG_SESSION_ID -u DBUS_SESSION_BUS_ADDRESS \
    HOME="$home" \
    XDG_CONFIG_HOME="$home/.config" \
    XDG_DATA_HOME="$home/.local/share" \
    XDG_CACHE_HOME="$home/.cache" \
    XDG_STATE_HOME="$home/.local/state" \
    XDG_RUNTIME_DIR="$runtime" \
    PATH="$home/.local/bin:$PATH" \
    dbus-run-session -- "$@" || status=$?

# The real desktop keeps running meanwhile (the real daemon rotating the
# wallpaper writes org/gnome/desktop/background), so show what changed rather
# than just that something did.
after=$(dconf dump / 2>/dev/null || true)
if [[ "$before" != "$after" ]]; then
    echo "NOTE: real settings changed during this run. A leak would mention $scratch:" >&2
    diff <(echo "$before") <(echo "$after") | grep '^[<>]' | head -20 >&2 || true
    if grep -qF "$scratch" <<<"$after"; then
        echo "WARNING: real settings now refer to the scratch folder: the session leaked." >&2
        status=99
    fi
fi
exit "$status"
