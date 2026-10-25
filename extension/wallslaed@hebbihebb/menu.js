// Items added to the desktop's right-click menu.

import {gettext as _} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as BackgroundMenu from 'resource:///org/gnome/shell/ui/backgroundMenu.js';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';

import {unpack} from './iface.js';

export class DesktopMenu {
    constructor(daemon, host, injections) {
        this._daemon = daemon;
        this._host = host;
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
        let items = menu._wallslaed;
        if (!items) {
            const section = new PopupMenu.PopupMenuSection();
            const add = (label, activate) => {
                const item = new PopupMenu.PopupMenuItem(label);
                item.connect('activate', activate);
                section.addMenuItem(item);
                return item;
            };
            const call = method => () => this._daemon.call(method);
            items = {
                section,
                playPause: add('', () => this._host.control('toggle')),
                mute: add('', () => this._host.control(this._host.media?.muted ? 'unmute' : 'mute')),
                stopMedia: add(_('Stop Music Video'), call('StopMedia')),
                next: add(_('Next Wallpaper'), call('Next')),
                previous: add(_('Previous Wallpaper'), call('Previous')),
                show: add(_('Show File Location'), call('ShowInFiles')),
                favorite: add('', call('ToggleFavorite')),
                ban: add(_('Never Show This Again'), call('Ban')),
                pause: add('', call('TogglePause')),
                music: new PopupMenu.PopupSubMenuMenuItem(_('Music Videos')),
            };
            items.music.visible = false;
            section.addMenuItem(items.music);
            // A separator inside the section would be hidden as its last item.
            items.separator = new PopupMenu.PopupSeparatorMenuItem();
            menu.addMenuItem(section, 0);
            menu.addMenuItem(items.separator, 1);
            menu._wallslaed = items;
            this._menus.set(menu, menu.connect('destroy', () => this._menus.delete(menu)));
        }
        this._refresh(menu);
        this._loadMusic(menu);
    }

    _refreshAll() {
        for (const menu of this._menus.keys())
            this._refresh(menu);
    }

    _refresh(menu) {
        const items = menu._wallslaed;
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
        for (const key of ['favorite', 'ban', 'pause'])
            items[key].visible = !music;
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

    async _loadMusic(menu) {
        const items = menu._wallslaed;
        let list = [];
        try {
            [list] = await this._daemon.request('ListMedia');
        } catch {
            // Daemon not running.
        }
        // The menu may have been destroyed (its wallpaper replaced) or cleaned
        // up while we waited.
        if (!this._menus.has(menu) || menu._wallslaed !== items)
            return;
        const active = this._daemon.state?.media;
        items.music.menu.removeAll();
        for (const {path, title} of list.map(unpack)) {
            const item = new PopupMenu.PopupMenuItem(title);
            item.setOrnament(path === active ? PopupMenu.Ornament.CHECK : PopupMenu.Ornament.NONE);
            item.connect('activate', () => this._daemon.call('PlayMedia', path));
            items.music.menu.addMenuItem(item);
        }
        items.music.visible = list.length > 0 && !!this._daemon.state;
    }

    destroy() {
        for (const [obj, id] of this._signals)
            obj.disconnect(id);
        this._signals = [];
        for (const [menu, id] of this._menus) {
            menu.disconnect(id);
            menu._wallslaed.section.destroy();
            menu._wallslaed.separator.destroy();
            delete menu._wallslaed;
        }
        this._menus.clear();
    }
}
