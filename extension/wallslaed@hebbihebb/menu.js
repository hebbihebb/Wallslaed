// Items added to the desktop's right-click menu.

import {gettext as _} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as BackgroundMenu from 'resource:///org/gnome/shell/ui/backgroundMenu.js';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';

export class DesktopMenu {
    constructor(daemon, injections) {
        this._daemon = daemon;
        this._menus = new Set();

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
            const add = (label, method) => {
                const item = new PopupMenu.PopupMenuItem(label);
                item.connect('activate', () => this._daemon.call(method));
                section.addMenuItem(item);
                return item;
            };
            items = {
                section,
                next: add(_('Next Wallpaper'), 'Next'),
                previous: add(_('Previous Wallpaper'), 'Previous'),
                show: add(_('Show File Location'), 'ShowInFiles'),
                favorite: add('', 'ToggleFavorite'),
                ban: add(_('Never Show This Again'), 'Ban'),
                pause: add('', 'TogglePause'),
            };
            // A separator inside the section would be hidden as its last item.
            items.separator = new PopupMenu.PopupSeparatorMenuItem();
            menu.addMenuItem(section, 0);
            menu.addMenuItem(items.separator, 1);
            menu._wallslaed = items;
            this._menus.add(menu);
            menu.connect('destroy', () => this._menus.delete(menu));
        }

        const state = this._daemon.state;
        const hasCurrent = !!state?.current;
        for (const key of ['next', 'previous', 'pause'])
            items[key].sensitive = !!state;
        for (const key of ['show', 'favorite', 'ban'])
            items[key].sensitive = hasCurrent;
        items.favorite.label.text = state?.favorite
            ? _('Remove from Favorites') : _('Add to Favorites');
        items.pause.label.text = state?.paused
            ? _('Resume Rotation') : _('Pause Rotation');
    }

    destroy() {
        for (const menu of this._menus) {
            menu._wallslaed.section.destroy();
            menu._wallslaed.separator.destroy();
            delete menu._wallslaed;
        }
        this._menus.clear();
    }
}
