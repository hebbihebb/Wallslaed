import {Extension, InjectionManager} from 'resource:///org/gnome/shell/extensions/extension.js';

import {AnimatedHost} from './animated.js';
import {BlurLayers} from './blur.js';
import {DaemonClient} from './daemon.js';
import {LiveBlur} from './liveBlur.js';
import {TopBar} from './topbar.js';
import {DesktopMenu} from './menu.js';
import {Transitions} from './transitions.js';

// All the real work (scanning, picking, decoding) happens in the wallslaed
// daemon. This extension only does what has to happen inside GNOME Shell.
export default class WallslaedExtension extends Extension {
    enable() {
        this._settings = this.getSettings();
        this._injections = new InjectionManager();
        this._daemon = new DaemonClient();
        this._animated = new AnimatedHost(this._settings, this._daemon);
        this._menu = new DesktopMenu(this._daemon, this._animated, this._injections);
        this._transitions = new Transitions(this._settings, this._injections);
        this._blur = new BlurLayers(this._settings, this._daemon);
        this._liveBlur = new LiveBlur(this._settings, this._injections);
        this._topBar = new TopBar(this._settings, this._injections);
    }

    disable() {
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
        this._settings = null;
    }
}
