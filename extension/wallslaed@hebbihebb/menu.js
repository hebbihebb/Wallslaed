// Wallslæd's controls, in two places: the desktop's right-click menu and a
// Quick Settings tile. The tile is the only way in where a desktop-icons
// extension (Ubuntu's DING, for one) covers the desktop and shows its own
// right-click menu instead of GNOME's.

import GLib from 'gi://GLib';
import GObject from 'gi://GObject';

import {gettext as _} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as BackgroundMenu from 'resource:///org/gnome/shell/ui/backgroundMenu.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';
import * as QuickSettings from 'resource:///org/gnome/shell/ui/quickSettings.js';

import {unpack} from './iface.js';

const ICON = 'preferences-desktop-wallpaper-symbolic';

// GNOME's own names for the power modes.
function modeName(mode) {
    return {
        'power-saver': _('Power Saver'),
        'balanced': _('Balanced'),
        'performance': _('Performance'),
    }[mode] ?? mode;
}

/**
 * One set of control items in a PopupMenuSection, kept current while the
 * daemon's state and the music player change. `leave` runs before an item
 * opens something outside the shell (a file manager, the preferences).
 */
class Controls {
    constructor(daemon, host, openPreferences, {leave = () => {}, pause = true} = {}) {
        this._daemon = daemon;
        this._host = host;
        this.section = new PopupMenu.PopupMenuSection();
        const add = (label, activate) => {
            const item = new PopupMenu.PopupMenuItem(label);
            item.connect('activate', activate);
            this.section.addMenuItem(item);
            return item;
        };
        const call = method => () => daemon.call(method);
        const away = action => () => {
            leave();
            action();
        };
        this._items = {
            playPause: add('', () => host.control('toggle')),
            mute: add('', () => host.control(host.media?.muted ? 'unmute' : 'mute')),
            stopMedia: add(_('Stop Music Video'), call('StopMedia')),
            next: add(_('Next Wallpaper'), call('Next')),
            previous: add(_('Previous Wallpaper'), call('Previous')),
            show: add(_('Show File Location'), away(call('ShowInFiles'))),
            favorite: add('', call('ToggleFavorite')),
            ban: add(_('Never Show This Again'), call('Ban')),
            pause: add('', call('TogglePause')),
            music: new PopupMenu.PopupSubMenuMenuItem(_('Music Videos')),
        };
        // The tile's own button pauses, so its menu doesn't repeat it.
        this._pause = pause;
        this._items.music.visible = false;
        this.section.addMenuItem(this._items.music);
        this._items.preferences = add(_('Wallslæd Settings'), away(openPreferences));
        this._musicGeneration = 0;
        this.refresh();
    }

    refresh() {
        const items = this._items;
        const state = this._daemon.state;
        const music = !!state?.media;
        const player = this._host.media; // null until the player is up
        const hasCurrent = !!state?.current;

        for (const key of ['playPause', 'mute', 'stopMedia'])
            items[key].visible = music;
        items.playPause.sensitive = items.mute.sensitive = !!player;
        items.playPause.label.text = player?.playing === false ? _('Play Music') : _('Pause Music');
        items.mute.label.text = player?.muted ? _('Unmute') : _('Mute');

        // Favorites, bans and the timer belong to the rotation, which a music
        // video puts on hold.
        for (const key of ['favorite', 'ban'])
            items[key].visible = !music;
        items.pause.visible = !music && this._pause;
        for (const key of ['next', 'previous', 'pause'])
            items[key].sensitive = !!state;
        for (const key of ['favorite', 'ban'])
            items[key].sensitive = hasCurrent;
        items.show.sensitive = hasCurrent || music;
        items.favorite.label.text = state?.favorite
            ? _('Remove from Favorites') : _('Add to Favorites');
        items.pause.label.text = state?.paused
            ? _('Resume Rotation') : _('Pause Rotation');
        if (!state)
            items.music.visible = false;
    }

    /** Fills the Music Videos submenu; call when the menu opens. */
    async loadMusic() {
        const generation = ++this._musicGeneration;
        let list = [];
        try {
            [list] = await this._daemon.request('ListMedia');
        } catch {
            // Daemon not running, or the client is gone.
        }
        // Destroyed, or opened again, while we waited.
        if (!this.section || generation !== this._musicGeneration)
            return;
        const music = this._items.music;
        const active = this._daemon.state?.media;
        music.menu.removeAll();
        for (const {path, title} of list.map(unpack)) {
            const item = new PopupMenu.PopupMenuItem(title);
            item.setOrnament(path === active ? PopupMenu.Ornament.CHECK : PopupMenu.Ornament.NONE);
            item.connect('activate', () => this._daemon.call('PlayMedia', path));
            music.menu.addMenuItem(item);
        }
        music.visible = list.length > 0 && !!this._daemon.state;
    }

    destroy() {
        this.section.destroy();
        this.section = null;
    }
}

/** Adds the controls to the desktop's right-click menu. */
export class DesktopMenu {
    constructor(daemon, host, injections, openPreferences) {
        this._daemon = daemon;
        this._host = host;
        this._openPreferences = openPreferences;
        this._menus = new Map(); // menu -> its destroy handler id

        // Menus can stay open while things change; keep them current.
        this._signals = [
            [daemon, daemon.connect('changed', () => this._refreshAll())],
            [host, host.connect('media-changed', () => this._refreshAll())],
        ];

        // GNOME builds a new BackgroundMenu every time the background changes,
        // so add our items lazily, the first time each one opens.
        const self = this;
        injections.overrideMethod(BackgroundMenu.BackgroundMenu.prototype, 'open',
            original => function (...args) {
                self._decorate(this);
                return original.apply(this, args);
            });
    }

    _decorate(menu) {
        let added = menu._wallslaed;
        if (!added) {
            added = {
                controls: new Controls(this._daemon, this._host, this._openPreferences),
                // A separator inside the section would be hidden as its last item.
                separator: new PopupMenu.PopupSeparatorMenuItem(),
            };
            // GNOME's own "Settings" below opens GNOME Settings, not ours.
            menu.addMenuItem(added.controls.section, 0);
            menu.addMenuItem(added.separator, 1);
            menu._wallslaed = added;
            this._menus.set(menu, menu.connect('destroy', () => {
                this._menus.delete(menu);
                added.controls.section = null;
            }));
        }
        added.controls.refresh();
        added.controls.loadMusic();
    }

    _refreshAll() {
        for (const menu of this._menus.keys())
            menu._wallslaed.controls.refresh();
    }

    destroy() {
        for (const [obj, id] of this._signals)
            obj.disconnect(id);
        this._signals = [];
        for (const [menu, id] of this._menus) {
            menu.disconnect(id);
            menu._wallslaed.controls.destroy();
            menu._wallslaed.separator.destroy();
            delete menu._wallslaed;
        }
        this._menus.clear();
    }
}

const Tile = GObject.registerClass(
class WallslaedTile extends QuickSettings.QuickMenuToggle {
    _init(daemon, host, power, openPreferences) {
        super._init({title: 'Wallslæd', iconName: ICON, toggleMode: false});
        this._daemon = daemon;
        this._host = host;
        this._power = power;
        this._controls = new Controls(daemon, host, openPreferences, {
            leave: () => Main.panel.closeQuickSettings(),
            pause: false,
        });
        this.menu.setHeader(ICON, 'Wallslæd');
        this.menu.addMenuItem(this._controls.section);

        this.connect('clicked', () => {
            if (this._daemon.state?.media)
                this._host.control('toggle');
            else if (this._daemon.state)
                this._daemon.call('TogglePause');
        });
        this.menu.connect('open-state-changed', (_menu, open) => {
            if (open)
                this._controls.loadMusic();
        });
        this._signals = [
            [daemon, daemon.connect('changed', () => this._sync())],
            [host, host.connect('media-changed', () => this._sync())],
            [power, power.connect('changed', () => this._sync())],
        ];
        // GNOME can dispose the tile without calling destroy() (shell
        // shutdown, a parent destroyed first): clean up on the signal too.
        this.connect('destroy', () => this._cleanup());
        this._sync();
    }

    _sync() {
        const state = this._daemon.state;
        const player = this._host.media;
        let subtitle;
        if (!state) {
            this.checked = false;
            subtitle = _('Not Running');
        } else if (state.media) {
            this.checked = player?.playing !== false;
            subtitle = _('Music Video');
        } else {
            this.checked = !state.paused;
            // Say when the power mode lightens the effects.
            subtitle = state.paused ? _('Paused')
                : this._power.reduced ? _('%s: lighter effects').format(modeName(this._power.mode))
                    : null;
        }
        this.subtitle = subtitle;
        const current = state?.['media-title'] ||
            (state?.current ? GLib.path_get_basename(state.current) : '');
        this.menu.setHeader(ICON, 'Wallslæd', current);
        this._controls.refresh();
    }

    _cleanup() {
        if (!this._signals)
            return;
        for (const [obj, id] of this._signals)
            obj.disconnect(id);
        this._signals = null;
        // The section goes with the menu; a pending ListMedia reply sees null.
        this._controls.section = null;
        // Quick Settings parents the menu to its own overlay, not to the
        // tile, so destroying the tile alone leaves the menu behind.
        this.menu.close(false);
        this.menu.destroy();
    }

    destroy() {
        this._cleanup();
        super.destroy();
    }
});

/** The Quick Settings tile: click pauses, the arrow opens the controls. */
export class QuickTile {
    constructor(daemon, host, power, openPreferences) {
        this._indicator = new QuickSettings.SystemIndicator();
        this._indicator.quickSettingsItems.push(new Tile(daemon, host, power, openPreferences));
        Main.panel.statusArea.quickSettings.addExternalIndicator(this._indicator);
    }

    destroy() {
        for (const item of this._indicator.quickSettingsItems)
            item.destroy();
        this._indicator.destroy();
        this._indicator = null;
    }
}
