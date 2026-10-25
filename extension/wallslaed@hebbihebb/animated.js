// Hosts animated wallpapers. wallslaed-player draws the video into one window
// per monitor; we keep those windows minimized and out of every window list,
// and show them through clones inside the desktop background actors. The player
// only runs while an animated wallpaper is current, and it gets paused whenever
// nobody can see it. Music videos are the exception: they play with sound, and
// only the user pauses them.

import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Meta from 'gi://Meta';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as Signals from 'resource:///org/gnome/shell/misc/signals.js';

const TITLE_PREFIX = 'wallslaed-player-';

const UPowerIface = `
<node>
  <interface name="org.freedesktop.UPower">
    <property name="OnBattery" type="b" access="read"/>
  </interface>
</node>`;
const UPowerProxy = Gio.DBusProxy.makeProxyWrapper(UPowerIface);

function playerPath() {
    const local = GLib.build_filenamev([GLib.get_home_dir(), '.local', 'bin', 'wallslaed-player']);
    if (GLib.file_test(local, GLib.FileTest.IS_EXECUTABLE))
        return local;
    return GLib.find_program_in_path('wallslaed-player');
}

/**
 * Emits 'media-changed' when a music video starts, stops, or its play/mute
 * state changes. `media` is {playing, muted} while one plays, null otherwise.
 */
export class AnimatedHost extends Signals.EventEmitter {
    constructor(settings, daemon) {
        super();
        this._settings = settings;
        this._daemon = daemon;
        this._player = null;
        // Players told to quit that haven't exited yet.
        this._retiring = new Set();
        this._clones = new Map(); // monitor index -> Clutter.Clone
        this._bgSignals = [];
        this._signals = [];
        this._checkId = 0;
        this._paused = false;
        this.media = null;

        const on = (obj, signal, fn) => this._signals.push([obj, obj.connect(signal, fn)]);
        const check = () => this._queueCheck();

        on(daemon, 'changed', () => this._sync());
        on(settings, 'changed', (_s, key) => {
            if (key === 'animated-enabled')
                this._sync();
            else if (key === 'music-volume')
                this._sendVolume();
            else if (key.startsWith('animated-'))
                check();
        });
        on(global.window_manager, 'map', (_wm, actor) => this._onMap(actor));
        on(Main.layoutManager, 'monitors-changed', () => this._restart());
        on(global.display, 'restacked', check);
        on(global.display, 'in-fullscreen-changed', check);
        on(global.window_manager, 'size-changed', check);
        on(global.window_manager, 'minimize', check);
        on(global.window_manager, 'unminimize', check);
        on(global.workspace_manager, 'active-workspace-changed', check);

        this._upower = new UPowerProxy(Gio.DBus.system, 'org.freedesktop.UPower',
            '/org/freedesktop/UPower', (_p, error) => {
                if (!error)
                    check();
            });
        this._signals.push([this._upower, this._upower.connect('g-properties-changed', check)]);

        this._sync();
    }

    /** Send a playback command (play, pause, toggle, mute, unmute) to a music video. */
    control(command) {
        if (this._player?.audio)
            this._send(this._player, command);
    }

    _owns(window, player = this._player) {
        try {
            return !!player?.client.owns_window(window);
        } catch {
            return false;
        }
    }

    _sync() {
        const state = this._daemon.state;
        if (state?.audio && state.playable)
            this._start(state.playable, state);
        else if (state?.animated && state.playable && this._settings.get_boolean('animated-enabled'))
            this._start(state.playable, null);
        else
            this._stop();
    }

    _restart() {
        this._stop();
        this._sync();
    }

    _start(file, media) {
        const audio = !!media;
        if (this._player?.file === file && this._player.audio === audio)
            return;
        this._stop();

        const path = playerPath();
        if (!path) {
            console.warn('Wallslæd: wallslaed-player not found; showing a still frame');
            return;
        }
        const argv = [path, file];
        if (audio) {
            argv.push('--audio', '--start', String(media['media-position'] ?? 0));
            if (media['media-title'])
                argv.push('--title', media['media-title']);
            if (media['media-art'])
                argv.push('--art', media['media-art']);
        }

        const launcher = new Gio.SubprocessLauncher({
            flags: Gio.SubprocessFlags.STDIN_PIPE | Gio.SubprocessFlags.STDOUT_PIPE,
        });
        let client;
        try {
            client = Meta.WaylandClient.new_subprocess(global.context, launcher, argv);
        } catch (e) {
            console.warn(`Wallslæd: could not start player: ${e.message}`);
            return;
        }
        const proc = client.get_subprocess();
        const player = {
            file, audio, client, proc,
            stdin: proc.get_stdin_pipe(),
            queue: [],
            writing: false,
            closing: false,
            exited: false,
            cancellable: new Gio.Cancellable(),
            actors: new Map(), // monitor index -> Meta.WindowActor
            windowSignals: [],
        };
        this._player = player;
        this._paused = false;
        proc.wait_async(null, (p, res) => {
            try {
                p.wait_finish(res);
            } catch {
                // Exited either way.
            }
            player.exited = true;
            this._retiring.delete(player);
            if (this._player === player)
                this._stop();
        });
        this._readState(player, new Gio.DataInputStream({base_stream: proc.get_stdout_pipe()}));
        if (audio) {
            this.media = {playing: true, muted: false};
            this._sendVolume();
            this.emit('media-changed');
        }
        this._watchBackgrounds();
    }

    _stop() {
        const player = this._player;
        this._player = null;
        this._detachClones();
        this._unwatchBackgrounds();
        if (this.media) {
            this.media = null;
            this.emit('media-changed');
        }
        if (!player)
            return;
        for (const [obj, id] of player.windowSignals)
            obj.disconnect(id);
        player.windowSignals = [];
        player.cancellable.cancel();
        // The player quits on 'quit' or end of input, and exits by itself within
        // two seconds even if something hangs, so there is nothing to time here.
        player.closing = true;
        this._send(player, 'quit');
        if (!player.exited)
            this._retiring.add(player);
    }

    _sendVolume() {
        this.control(`volume ${this._settings.get_double('music-volume')}`);
    }

    // GIO allows one pending write per stream, so commands go out one at a
    // time, in order. Once a closing player's queue drains, stdin is closed.
    _send(player, command) {
        if (player.exited)
            return;
        player.queue.push(command);
        if (!player.writing)
            this._flush(player);
    }

    _flush(player) {
        const command = player.queue.shift();
        if (command === undefined) {
            player.writing = false;
            if (player.closing) {
                player.stdin.close_async(GLib.PRIORITY_DEFAULT, null, (stream, res) => {
                    try {
                        stream.close_finish(res);
                    } catch {
                        // Already gone.
                    }
                });
            }
            return;
        }
        player.writing = true;
        const bytes = new TextEncoder().encode(`${command}\n`);
        player.stdin.write_all_async(bytes, GLib.PRIORITY_DEFAULT, null, (stream, res) => {
            try {
                stream.write_all_finish(res);
            } catch (e) {
                if (!e.matches(Gio.IOErrorEnum, Gio.IOErrorEnum.BROKEN_PIPE) &&
                    !e.matches(Gio.IOErrorEnum, Gio.IOErrorEnum.CLOSED))
                    console.warn(`Wallslæd: player command failed: ${e.message}`);
                player.queue = [];
                player.writing = false;
                return;
            }
            this._flush(player);
        });
    }

    // The player reports "state <playing|paused> <muted 0|1>" on every change.
    _readState(player, input) {
        input.read_line_async(GLib.PRIORITY_DEFAULT, player.cancellable, (stream, res) => {
            let line;
            try {
                [line] = stream.read_line_finish_utf8(res);
            } catch {
                return; // Cancelled, or the player is gone.
            }
            if (line === null)
                return;
            const [word, playState, muted] = line.trim().split(' ');
            if (word === 'state' && player.audio && this._player === player) {
                this.media = {playing: playState === 'playing', muted: muted === '1'};
                this.emit('media-changed');
            }
            this._readState(player, stream);
        });
    }

    _onMap(actor) {
        const player = this._player;
        const window = actor.meta_window;
        if (!player || !this._owns(window)) {
            // A player on its way out may still map a window; never show it.
            for (const old of this._retiring) {
                if (this._owns(window, old)) {
                    actor.opacity = 0;
                    window.minimize();
                    break;
                }
            }
            return;
        }

        const index = parseInt(window.title?.slice(TITLE_PREFIX.length) ?? '', 10);
        if (Number.isNaN(index))
            return;

        // Never let the real window be seen or clicked; only its clones are.
        actor.remove_all_transitions();
        actor.opacity = 0;
        const monitor = Main.layoutManager.monitors[index];
        if (monitor)
            window.move_frame(false, monitor.x, monitor.y);
        // Mutter refuses to minimize windows hidden from the window list, so
        // minimize first, then hide it from the overview, dash and alt-tab.
        const tuck = () => {
            window.show_in_window_list();
            window.minimize();
            window.hide_from_window_list();
        };
        tuck();
        player.windowSignals.push([window, window.connect('notify::minimized', () => {
            if (!window.minimized)
                tuck();
        })]);
        player.windowSignals.push([window, window.connect('raised', () => window.lower())]);

        player.actors.set(index, actor);
        this._attach(index);
        this._queueCheck();
    }

    _watchBackgrounds() {
        this._unwatchBackgrounds();
        Main.layoutManager._bgManagers.forEach((mgr, index) => {
            this._bgSignals.push([mgr, mgr.connect('changed', () => this._attach(index))]);
        });
    }

    _unwatchBackgrounds() {
        for (const [obj, id] of this._bgSignals)
            obj.disconnect(id);
        this._bgSignals = [];
    }

    _attach(index) {
        const source = this._player?.actors.get(index);
        const mgr = Main.layoutManager._bgManagers[index];
        const monitor = Main.layoutManager.monitors[index];
        if (!source || !mgr || !monitor)
            return;

        this._clones.get(index)?.destroy();
        const clone = new Clutter.Clone({
            source,
            width: monitor.width,
            height: monitor.height,
        });
        clone.connect('destroy', () => {
            if (this._clones.get(index) === clone)
                this._clones.delete(index);
        });
        mgr.backgroundActor.add_child(clone);
        this._clones.set(index, clone);
    }

    _detachClones() {
        for (const clone of [...this._clones.values()])
            clone.destroy();
        this._clones.clear();
    }

    _queueCheck() {
        if (this._checkId || !this._player)
            return;
        this._checkId = GLib.idle_add(GLib.PRIORITY_LOW, () => {
            this._checkId = 0;
            this._updatePlayback();
            return GLib.SOURCE_REMOVE;
        });
    }

    _updatePlayback() {
        // Music videos keep playing behind windows; the user pauses them.
        if (!this._player || this._player.audio)
            return;
        const onBattery = this._settings.get_boolean('animated-pause-on-battery') &&
            this._upower?.OnBattery === true;
        const covered = this._settings.get_boolean('animated-pause-when-covered') &&
            this._desktopCovered();
        const pause = onBattery || covered;
        if (pause !== this._paused) {
            this._paused = pause;
            this._send(this._player, pause ? 'pause' : 'play');
        }
    }

    _desktopCovered() {
        const n = global.display.get_n_monitors();
        const covered = new Array(n).fill(false);
        const workspace = global.workspace_manager.get_active_workspace();
        for (const w of workspace.list_windows()) {
            if (w.minimized || !w.showing_on_its_workspace() || this._owns(w))
                continue;
            const m = w.get_monitor();
            if (m >= 0 && m < n && (w.is_fullscreen() || w.is_maximized()))
                covered[m] = true;
        }
        return covered.every(Boolean);
    }

    destroy() {
        if (this._checkId)
            GLib.source_remove(this._checkId);
        this._checkId = 0;
        this._stop();
        this._retiring.clear();
        for (const [obj, id] of this._signals)
            obj.disconnect(id);
        this._signals = [];
        this._upower = null;
    }
}
