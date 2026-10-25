import {Extension, InjectionManager} from 'resource:///org/gnome/shell/extensions/extension.js';

import {AnimatedHost} from './animated.js';
import {DaemonClient} from './daemon.js';
import {DesktopMenu} from './menu.js';
import {Transitions} from './transitions.js';

// All the real work (scanning, picking, decoding) happens in the wallslaed
// daemon. This extension only does what has to happen inside GNOME Shell.
export default class WallslaedExtension extends Extension {
    enable() {
        this._settings = this.getSettings();
        this._injections = new InjectionManager();
        this._daemon = new DaemonClient();
        this._menu = new DesktopMenu(this._daemon, this._injections);
        this._transitions = new Transitions(this._settings, this._injections);
        this._animated = new AnimatedHost(this._settings, this._daemon);
    }

    disable() {
        this._injections.clear();
        this._animated.destroy();
        this._transitions.destroy();
        this._menu.destroy();
        this._daemon.destroy();
        this._injections = null;
        this._animated = null;
        this._transitions = null;
        this._menu = null;
        this._daemon = null;
        this._settings = null;
    }
}
