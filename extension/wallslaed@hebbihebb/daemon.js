// Thin client for the wallslaed daemon's D-Bus API.

import Gio from 'gi://Gio';

import * as Signals from 'resource:///org/gnome/shell/misc/signals.js';

import {BUS_NAME, IFACE_XML, OBJECT_PATH, unpack} from './iface.js';

const WallslaedProxy = Gio.DBusProxy.makeProxyWrapper(IFACE_XML);

/**
 * Emits 'changed' whenever the daemon's state changes or it appears/vanishes.
 * `state` is null while the daemon isn't running.
 */
export class DaemonClient extends Signals.EventEmitter {
    constructor() {
        super();
        this.state = null;
        this._proxy = new WallslaedProxy(
            Gio.DBus.session, BUS_NAME, OBJECT_PATH,
            (proxy, error) => {
                if (error)
                    console.warn(`Wallslæd: ${error.message}`);
                else if (this._proxy)
                    this._refresh();
            },
            null, Gio.DBusProxyFlags.DO_NOT_AUTO_START);
        this._signalId = this._proxy.connectSignal('Changed',
            (_proxy, _sender, [state]) => this._set(unpack(state)));
        this._ownerId = this._proxy.connect('notify::g-name-owner', () => this._refresh());
    }

    get running() {
        return this._proxy?.g_name_owner != null;
    }

    async _refresh() {
        if (!this.running) {
            this._set(null);
            return;
        }
        try {
            const [state] = await this._proxy.GetStateAsync();
            if (this._proxy)
                this._set(unpack(state));
        } catch (e) {
            console.warn(`Wallslæd: GetState failed: ${e.message}`);
        }
    }

    _set(state) {
        this.state = state;
        this.emit('changed');
    }

    call(method, ...args) {
        this.request(method, ...args)
            .catch(e => console.warn(`Wallslæd: ${method} failed: ${e.message}`));
    }

    /** Like call(), but returns the reply (an array of out arguments). */
    request(method, ...args) {
        if (!this._proxy)
            return Promise.reject(new Error('client destroyed'));
        return this._proxy[`${method}Async`](...args);
    }

    destroy() {
        this._proxy.disconnectSignal(this._signalId);
        this._proxy.disconnect(this._ownerId);
        this._proxy = null;
        this.state = null;
    }
}
