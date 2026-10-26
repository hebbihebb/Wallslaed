import {Extension, InjectionManager} from 'resource:///org/gnome/shell/extensions/extension.js';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';

import {AnimatedHost} from './animated.js';
import {BlurLayers} from './blur.js';
import {DaemonClient} from './daemon.js';
import {LiveBlur} from './liveBlur.js';
import {LockBlur} from './lockBlur.js';
import {TopBar} from './topbar.js';
import {DesktopMenu} from './menu.js';
import {Transitions} from './transitions.js';

// All the real work (scanning, picking, decoding) happens in the wallslaed
// daemon. This extension only does what has to happen inside GNOME Shell.
//
// It stays enabled on the lock screen, but only for the lock screen's blur:
// everything else is torn down on lock and built again on unlock, exactly as
// if the shell had disabled the extension.
export default class WallslaedExtension extends Extension {
    enable() {
        this._settings = this.getSettings();
        this._lockInjections = new InjectionManager();
        this._lockBlur = new LockBlur(this._settings, this._lockInjections);
        this._sessionId = Main.sessionMode.connect('updated', () => this._syncSession());
        this._syncSession();
    }

    disable() {
        Main.sessionMode.disconnect(this._sessionId);
        this._disableDesktop();
        this._lockInjections.clear();
        this._lockBlur.destroy();
        this._lockInjections = null;
        this._lockBlur = null;
        this._settings = null;
    }

    _syncSession() {
        if (Main.sessionMode.isLocked)
            this._disableDesktop();
        else
            this._enableDesktop();
    }

    _enableDesktop() {
        if (this._daemon)
            return;
        this._injections = new InjectionManager();
        this._daemon = new DaemonClient();
        this._animated = new AnimatedHost(this._settings, this._daemon);
        this._menu = new DesktopMenu(this._daemon, this._animated, this._injections,
            () => this.openPreferences());
        this._transitions = new Transitions(this._settings, this._injections);
        this._blur = new BlurLayers(this._settings, this._daemon);
        this._liveBlur = new LiveBlur(this._settings, this._injections);
        this._topBar = new TopBar(this._settings, this._injections);
    }

    _disableDesktop() {
        if (!this._daemon)
            return;
        this._injections.clear();
        this._blur.destroy();
        this._liveBlur.destroy();
        this._topBar.destroy();
        this._menu.destroy();
        this._animated.destroy();
        this._transitions.destroy();
        this._daemon.destroy();
        this._injections = null;
        this._animated = null;
        this._transitions = null;
        this._blur = null;
        this._liveBlur = null;
        this._topBar = null;
        this._menu = null;
        this._daemon = null;
    }
}
