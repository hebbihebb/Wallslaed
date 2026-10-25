#!/usr/bin/env bash
# Build and install Wallslæd for the current user.
set -euo pipefail
cd "$(dirname "$0")"

UUID=wallslaed@hebbihebb
BIN="$HOME/.local/bin"
EXT="$HOME/.local/share/gnome-shell/extensions/$UUID"
SCHEMAS="$HOME/.local/share/glib-2.0/schemas"
UNITS="$HOME/.config/systemd/user"

cargo build --release

install -Dm755 -t "$BIN" target/release/wallslaed target/release/wallslaed-player
install -Dm644 -t "$SCHEMAS" data/io.github.hebbihebb.Wallslaed.gschema.xml
glib-compile-schemas "$SCHEMAS"

rm -rf "$EXT"
install -Dm644 -t "$EXT" extension/$UUID/*.js extension/$UUID/metadata.json
install -Dm644 -t "$EXT/schemas" data/io.github.hebbihebb.Wallslaed.gschema.xml
glib-compile-schemas "$EXT/schemas"

install -Dm644 -t "$UNITS" data/wallslaed.service
systemctl --user daemon-reload
systemctl --user enable wallslaed.service
systemctl --user restart wallslaed.service

if gnome-extensions list --enabled 2>/dev/null | grep -qiE 'azwallpaper|wallpaper-slideshow|variety'; then
    echo
    echo "Note: another wallpaper changer is enabled:"
    gnome-extensions list --enabled | grep -iE 'azwallpaper|wallpaper-slideshow|variety' | sed 's/^/  /'
    echo "They will fight over the background. Disable it with: gnome-extensions disable <uuid>"
fi

echo
if gnome-extensions enable "$UUID" 2>/dev/null; then
    echo "Done. Right-click the desktop to use Wallslæd."
else
    echo "Done. GNOME Shell only sees new extensions after you log out and back in."
    echo "Then run: gnome-extensions enable $UUID"
fi
