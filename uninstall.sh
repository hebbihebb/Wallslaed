#!/usr/bin/env bash
# Remove Wallslæd. Settings, favorites and history are kept unless --purge is given.
set -euo pipefail

UUID=wallslaed@hebbihebb
gnome-extensions disable "$UUID" 2>/dev/null || true
systemctl --user disable --now wallslaed.service 2>/dev/null || true

rm -rf "$HOME/.local/share/gnome-shell/extensions/$UUID"
rm -f "$HOME/.local/bin/wallslaed" "$HOME/.local/bin/wallslaed-player"
rm -f "$HOME/.config/systemd/user/wallslaed.service"
systemctl --user daemon-reload
rm -rf "$HOME/.cache/wallslaed"

if [[ "${1:-}" == "--purge" ]]; then
    dconf reset -f /io/github/hebbihebb/Wallslaed/ 2>/dev/null || true
    rm -rf "$HOME/.local/state/wallslaed"
    rm -f "$HOME/.local/share/glib-2.0/schemas/io.github.hebbihebb.Wallslaed.gschema.xml"
    glib-compile-schemas "$HOME/.local/share/glib-2.0/schemas" 2>/dev/null || true
fi
echo "Wallslæd removed. Your current wallpaper stays as it is."
