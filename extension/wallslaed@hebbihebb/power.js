// The system power mode, and what it allows of the three effects that cost
// something (measured in docs/how-it-works.md): menu and popup blur,
// transitions and animated wallpapers. Everything else costs about nothing
// and stays as set.

import Gio from 'gi://Gio';

import * as Signals from 'resource:///org/gnome/shell/misc/signals.js';

const ProfilesIface = `
<node>
  <interface name="org.freedesktop.UPower.PowerProfiles">
    <property name="ActiveProfile" type="s" access="read"/>
  </interface>
</node>`;
const ProfilesProxy = Gio.DBusProxy.makeProxyWrapper(ProfilesIface);

export const MODES = ['power-saver', 'balanced', 'performance'];
export const FEATURES = ['popups', 'transitions', 'animated'];
const LEVELS = ['full', 'light', 'off'];

/**
 * Emits 'changed' when the power mode or a level setting changes.
 * level(feature) is 'full', 'light' or 'off'.
 */
export class PowerMode extends Signals.EventEmitter {
    constructor(settings) {
        super();
        this._settings = settings;
        this._changed = () => this.emit('changed');
        this._settingsId = settings.connect('changed', (_s, key) => {
            if (key === 'power-adapt' || key === 'power-mode-override' ||
                MODES.some(mode => key.startsWith(`${mode}-`)))
                this._changed();
        });
        // power-profiles-daemon and tuned-ppd both answer here. Without
        // either, there is no mode to follow and every effect stays full.
        this._proxy = new ProfilesProxy(Gio.DBus.system, 'org.freedesktop.UPower.PowerProfiles',
            '/org/freedesktop/UPower/PowerProfiles', (_proxy, error) => {
                if (!error && this._proxy)
                    this._changed();
            }, null, Gio.DBusProxyFlags.DO_NOT_AUTO_START);
        this._proxyId = this._proxy.connect('g-properties-changed', this._changed);
    }

    /** 'power-saver', 'balanced', 'performance', or null if unknown. */
    get mode() {
        const override = this._settings.get_string('power-mode-override');
        if (MODES.includes(override))
            return override;
        const active = this._proxy?.ActiveProfile;
        return MODES.includes(active) ? active : null;
    }

    level(feature) {
        const mode = this.mode;
        if (!mode || !this._settings.get_boolean('power-adapt'))
            return 'full';
        const level = this._settings.get_string(`${mode}-${feature}`);
        return LEVELS.includes(level) ? level : 'full';
    }

    /** Whether the current mode turns any effect down. */
    get reduced() {
        return FEATURES.some(feature => this.level(feature) !== 'full');
    }

    destroy() {
        this._settings.disconnect(this._settingsId);
        this._proxy.disconnect(this._proxyId);
        this._proxy = null;
    }
}
