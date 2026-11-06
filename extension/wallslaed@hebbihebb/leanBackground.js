// GNOME's own Background (ui/background.js) creates a Gio.Settings and a
// GnomeDesktop.WallClock, which holds another, every time the wallpaper
// changes. The old ones become garbage, and the garbage collector freeing a
// GSettings while dconf delivers a change is what deadlocks GJS 1.90
// (docs/gnome-gjs-deadlock.md). So for an ordinary picture on the desktop,
// GNOME gets a lean background instead: a Meta.Background with the signals
// and fields GNOME's code uses ('loaded', 'bg-changed', isLoaded,
// updateResolution, destroy), listening to the settings GNOME's
// BackgroundSource already holds for the session, and creating none.
//
// GNOME keeps the rest: it still decides when to swap, reads the settings and
// builds the actors, so transitions, the overview, the lock screen and the
// blur work unchanged. Slideshow XML files, no picture at all, and GNOME's
// performance-test override still get GNOME's own Background. Unlike GNOME's,
// a lean background doesn't reload a picture rewritten in place: the daemon
// writes new files under new names.

import Cogl from 'gi://Cogl';
import GDesktopEnums from 'gi://GDesktopEnums';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import GObject from 'gi://GObject';
import Meta from 'gi://Meta';

import * as Background from 'resource:///org/gnome/shell/ui/background.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';

import {loadInto} from './compat.js';

const DESKTOP_SCHEMA = 'org.gnome.desktop.background';

const LeanBackground = GObject.registerClass({
    Signals: {'loaded': {}, 'bg-changed': {}},
}, class LeanBackground extends Meta.Background {
    // `settings` and `interfaceSettings` belong to GNOME's BackgroundSource;
    // `take(file)` gives a prepared texture, or null.
    _init({file, style, settings, interfaceSettings, take}) {
        super._init({meta_display: global.display});
        this.isLoaded = false;
        this._file = file;
        this._style = style;
        this._settings = settings;
        this._interfaceSettings = interfaceSettings;
        this._cancellable = new Gio.Cancellable();
        this._idleIds = new Set();
        // As GNOME's: any change to the background settings or the colour
        // scheme makes GNOME build a new one.
        this._settingsId = settings.connect('changed', () => this._emitChanged());
        this._schemeId = interfaceSettings.connect('changed::color-scheme', () => this._emitChanged());
        this._loadPattern();
        this._load(take);
    }

    updateResolution() {
        // Only slideshows depend on the monitor; they never get here.
    }

    destroy() {
        this._cancellable.cancel();
        this._settings.disconnect(this._settingsId);
        this._interfaceSettings.disconnect(this._schemeId);
        for (const id of this._idleIds)
            GLib.source_remove(id);
        this._idleIds.clear();
    }

    _idle(fn) {
        const id = GLib.idle_add(GLib.PRIORITY_DEFAULT, () => {
            this._idleIds.delete(id);
            fn();
            return GLib.SOURCE_REMOVE;
        });
        this._idleIds.add(id);
        return id;
    }

    _emitChanged() {
        if (this._changedIdle)
            return;
        this._changedIdle = this._idle(() => {
            this._changedIdle = 0;
            this.emit('bg-changed');
        });
    }

    _loadPattern() {
        const [, color] = Cogl.Color.from_string(this._settings.get_string('primary-color'));
        const [, second] = Cogl.Color.from_string(this._settings.get_string('secondary-color'));
        const shading = this._settings.get_enum('color-shading-type');
        if (shading === GDesktopEnums.BackgroundShading.SOLID)
            this.set_color(color);
        else
            this.set_gradient(shading, color, second);
    }

    async _load(take) {
        try {
            const entry = await take(this._file);
            if (this._cancellable.is_cancelled())
                return;
            if (entry?.texture)
                this.set_texture(entry.texture, this._style, entry.colorState);
            else
                await loadInto(this, this._file, this._style, this._cancellable);
        } catch (e) {
            if (!e.matches?.(Gio.IOErrorEnum, Gio.IOErrorEnum.CANCELLED))
                console.warn(`Wallslæd: can't load ${this._file.get_uri()}: ${e.message}`);
        }
        // As GNOME's: loaded even if it failed, so nothing waits for ever.
        if (this.isLoaded || this._cancellable.is_cancelled())
            return;
        this.isLoaded = true;
        this._idle(() => this.emit('loaded'));
    }
});

export class LeanBackgrounds {
    // `preloader()`: the desktop's Preloader while there is one, else null.
    constructor(injections, preloader) {
        this._injections = injections;
        this._preloader = preloader;
        this._made = new Set();
        this._hooked = false;

        // BackgroundSource isn't exported; reach it through an instance, now
        // or before the first background actor is made.
        const self = this;
        injections.overrideMethod(Background.BackgroundManager.prototype, '_createBackgroundActor',
            original => function (...args) {
                self._hook(this._backgroundSource);
                return original.apply(this, args);
            });
        this._hook(Main.layoutManager._bgManagers[0]?._backgroundSource);
    }

    _hook(source) {
        if (this._hooked || typeof source?.getBackground !== 'function')
            return;
        this._hooked = true;
        const self = this;
        this._injections.overrideMethod(Object.getPrototypeOf(source), 'getBackground',
            original => function (monitorIndex) {
                return self._get(this, monitorIndex) ?? original.call(this, monitorIndex);
            });
    }

    // GNOME's getBackground for an ordinary picture, with a lean background;
    // null for anything else, which GNOME handles itself.
    _get(source, monitorIndex) {
        const settings = source._settings;
        if (settings?.schema_id !== DESKTOP_SCHEMA || source._overrideImage != null || !source._interfaceSettings)
            return null;
        const style = settings.get_enum('picture-options');
        if (style === GDesktopEnums.BackgroundStyle.NONE)
            return null;
        const dark = source._interfaceSettings.get_enum('color-scheme') === GDesktopEnums.ColorScheme.PREFER_DARK;
        const uri = settings.get_string(dark ? 'picture-uri-dark' : 'picture-uri');
        if (!uri)
            return null;
        const file = Gio.File.new_for_commandline_arg(uri);
        if (file.get_basename().endsWith('.xml'))
            return null;
        // One background for every monitor, as GNOME does for a picture.
        if (0 in source._backgrounds)
            return source._backgrounds[0];

        const background = new LeanBackground({
            file,
            style,
            settings,
            interfaceSettings: source._interfaceSettings,
            take: f => this._preloader()?.take(f) ?? Promise.resolve(null),
        });
        // As GNOME's: dropped from the source when it changes.
        background._changedId = background.connect('bg-changed', () => {
            background.disconnect(background._changedId);
            background.destroy();
            if (source._backgrounds?.[0] === background)
                delete source._backgrounds[0];
            this._made.delete(background);
        });
        source._backgrounds[0] = background;
        this._made.add(background);
        return background;
    }

    /** After the injections are cleared: let GNOME replace ours with its own. */
    destroy() {
        // Each one's 'bg-changed' makes GNOME drop it and build its own.
        for (const background of [...this._made])
            background.emit('bg-changed');
        this._made.clear();
    }
}
