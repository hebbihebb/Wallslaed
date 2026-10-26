// Uploads the next wallpaper ahead of time, so changing it doesn't stall the
// compositor. GNOME 51 decodes a new wallpaper synchronously and uploads it to
// the GPU in one call, on the compositor's own thread: 125-155 ms at 4K, even
// with transitions off. The daemon announces the picture it will show next
// (`next-picture`); it is decoded here through glycin's async loaders and
// uploaded a few rows at a time while the shell is idle. When GNOME then loads
// that file, it gets the finished texture instead of loading it itself.
//
// Everything else stays GNOME's: the settings, its Background objects, the
// actor swap the transitions hook into. A file this didn't prepare (the first
// one after login, Previous, a format it doesn't handle) loads as before.
// Textures prepared here never enter GNOME's texture cache, which keeps every
// wallpaper ever shown on the GPU; this keeps only the current and next ones.

import Clutter from 'gi://Clutter';
import Cogl from 'gi://Cogl';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Glycin from 'gi://Gly';

import * as Background from 'resource:///org/gnome/shell/ui/background.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';

Gio._promisify(Glycin.Loader.prototype, 'load_async');
Gio._promisify(Glycin.Image.prototype, 'next_frame_async');
Gio._promisify(Gio.File.prototype, 'query_info_async');

// Upload in short bursts so a frame is never held up for long: rows per
// set_region call, compositor time per burst, and the gap between bursts.
const ROWS = 16;
const BURST_MS = 2;
const INTERVAL_MS = 8;

// The 8-bit memory formats GNOME accepts from glycin. GNOME also accepts 16-bit
// and float ones, so glycin is asked for all of them (never converting an
// image GNOME would show at full depth), and those are left to GNOME.
const M = Glycin.MemoryFormat;
const S = Glycin.MemoryFormatSelection;
const P = Cogl.PixelFormat;
const FORMATS = [
    [M.R8G8B8, S.R8G8B8, P.RGB_888, false],
    [M.B8G8R8, S.B8G8R8, P.BGR_888, false],
    [M.R8G8B8A8, S.R8G8B8A8, P.RGBA_8888, true],
    [M.B8G8R8A8, S.B8G8R8A8, P.BGRA_8888, true],
    [M.A8R8G8B8, S.A8R8G8B8, P.ARGB_8888, true],
    [M.A8B8G8R8, S.A8B8G8R8, P.ABGR_8888, true],
    [M.R8G8B8A8_PREMULTIPLIED, S.R8G8B8A8_PREMULTIPLIED, P.RGBA_8888_PRE, true],
    [M.B8G8R8A8_PREMULTIPLIED, S.B8G8R8A8_PREMULTIPLIED, P.BGRA_8888_PRE, true],
    [M.A8R8G8B8_PREMULTIPLIED, S.A8R8G8B8_PREMULTIPLIED, P.ARGB_8888_PRE, true],
];
const ACCEPTED = FORMATS.reduce((all, [, selection]) => all | selection,
    S.R16G16B16A16_PREMULTIPLIED | S.R16G16B16A16 | S.R16G16B16A16_FLOAT |
    S.R32G32B32A32_FLOAT_PREMULTIPLIED | S.R32G32B32A32_FLOAT);
const COGL_FORMATS = new Map(FORMATS.map(([memory, , cogl, alpha]) => [memory, [cogl, alpha]]));

const now = () => GLib.get_monotonic_time() / 1000;

export class Preloader {
    constructor(daemon, injections, busy) {
        this._daemon = daemon;
        this._injections = injections;
        // True while a transition runs; uploads wait for it to end.
        this._busy = busy;
        // uri -> Entry: the next picture, and the current one once shown.
        this._entries = new Map();
        this._hooked = false;

        // GNOME's Background class isn't exported; reach it through an
        // instance, now or when the first background actor is created.
        const self = this;
        injections.overrideMethod(Background.BackgroundManager.prototype, '_createBackgroundActor',
            original => function (...args) {
                const actor = original.apply(this, args);
                self._hook(actor.content.background);
                return actor;
            });
        this._hook(Main.layoutManager._bgManagers[0]?.backgroundActor?.content.background);

        this._changedId = daemon.connect('changed', () => this._sync());
        this._sync();
    }

    _hook(background) {
        if (this._hooked || typeof background?._loadImage !== 'function')
            return;
        this._hooked = true;
        const self = this;
        this._injections.overrideMethod(Object.getPrototypeOf(background), '_loadImage',
            original => async function (file) {
                const entry = await self._take(file);
                if (!entry)
                    return original.call(this, file);
                // As GNOME's own _loadImage does with a texture from its cache.
                this._watchFile(file);
                if (!this._cancellable.is_cancelled())
                    this.set_texture(entry.texture, this._style, entry.colorState);
                this._setLoaded();
                return undefined;
            });
    }

    // The finished texture for `file`, or null to let GNOME load it.
    async _take(file) {
        const entry = this._entries.get(file.get_uri());
        if (!entry)
            return null;
        try {
            await entry.ready;
            // Rewritten since it was prepared (GNOME reloads on file changes).
            const info = await file.query_info_async(Gio.FILE_ATTRIBUTE_ETAG_VALUE,
                Gio.FileQueryInfoFlags.NONE, GLib.PRIORITY_DEFAULT, null);
            if (info.get_etag() !== entry.etag)
                throw new Error('changed on disk');
            return entry.texture ? entry : null;
        } catch (e) {
            if (!e.matches?.(Gio.IOErrorEnum, Gio.IOErrorEnum.CANCELLED))
                console.warn(`Wallslæd: not preloaded ${file.get_path()}: ${e.message}`);
            this._drop(file.get_uri());
            return null;
        }
    }

    // Keep the current and next pictures, prepare the next one.
    _sync() {
        const state = this._daemon.state;
        const uri = path => path ? Gio.File.new_for_path(path).get_uri() : null;
        const current = uri(state?.picture);
        const next = uri(state?.['next-picture']);
        for (const key of [...this._entries.keys()]) {
            if (key !== current && key !== next)
                this._drop(key);
        }
        if (next && !this._entries.has(next))
            this._entries.set(next, new Entry(Gio.File.new_for_uri(next), this._busy));
    }

    _drop(uri) {
        this._entries.get(uri)?.destroy();
        this._entries.delete(uri);
    }

    destroy() {
        this._daemon.disconnect(this._changedId);
        for (const entry of this._entries.values())
            entry.destroy();
        this._entries.clear();
        // The injections are cleared by the extension.
    }
}

// One picture: decoded off the compositor thread, uploaded a burst at a time.
class Entry {
    constructor(file, busy) {
        this.texture = null;
        this.colorState = null;
        this.etag = null;
        this._busy = busy;
        this._cancellable = new Gio.Cancellable();
        this._timeoutId = 0;
        this.ready = this._load(file);
        // _take() reports failures; don't leave this one unhandled meanwhile.
        this.ready.catch(() => {});
    }

    async _load(file) {
        const cancellable = this._cancellable;
        const info = await file.query_info_async(Gio.FILE_ATTRIBUTE_ETAG_VALUE,
            Gio.FileQueryInfoFlags.NONE, GLib.PRIORITY_LOW, cancellable);
        this.etag = info.get_etag();
        const stream = await file.read_async(GLib.PRIORITY_LOW, cancellable);
        const loader = Glycin.Loader.new_for_stream(stream);
        loader.set_accepted_memory_formats(ACCEPTED);
        const image = await loader.load_async(cancellable);
        const frame = await image.next_frame_async(cancellable);

        const [coglFormat, alpha] = COGL_FORMATS.get(frame.get_memory_format()) ?? [];
        if (coglFormat === undefined)
            throw new Error(`memory format ${frame.get_memory_format()} left to GNOME`);
        const colorState = colorStateOf(frame);

        const width = frame.get_width();
        const height = frame.get_height();
        const context = global.stage.context.get_backend().get_cogl_context();
        const texture = Cogl.Texture2D.new_with_size(context, width, height);
        texture.set_components(alpha ? Cogl.TextureComponents.RGBA : Cogl.TextureComponents.RGB);
        // Too large for one texture: GNOME falls back to a sliced one.
        texture.allocate();

        // Dropped while glycin finished decoding.
        cancellable.set_error_if_cancelled();
        await this._upload(texture, frame, coglFormat);
        this.texture = texture;
        this.colorState = colorState;
    }

    _upload(texture, frame, format) {
        const width = frame.get_width();
        const height = frame.get_height();
        const stride = frame.get_stride();
        const bytes = frame.get_buf_bytes();
        const size = bytes.get_size();
        let y = 0;
        return new Promise((resolve, reject) => {
            this._reject = reject;
            this._timeoutId = GLib.timeout_add(GLib.PRIORITY_LOW, INTERVAL_MS, () => {
                if (this._busy())
                    return GLib.SOURCE_CONTINUE;
                const start = now();
                try {
                    while (y < height && now() - start < BURST_MS) {
                        const rows = Math.min(ROWS, height - y);
                        const offset = y * stride;
                        // Only this slice is copied out of the frame.
                        const data = GLib.Bytes.new_from_bytes(bytes, offset,
                            Math.min(rows * stride, size - offset)).get_data();
                        texture.set_region(0, 0, 0, y, width, rows, width, rows, format, stride, data);
                        y += rows;
                    }
                } catch (e) {
                    this._timeoutId = 0;
                    reject(e);
                    return GLib.SOURCE_REMOVE;
                }
                if (y < height)
                    return GLib.SOURCE_CONTINUE;
                this._timeoutId = 0;
                resolve();
                return GLib.SOURCE_REMOVE;
            });
        });
    }

    destroy() {
        this._cancellable.cancel();
        if (this._timeoutId) {
            GLib.source_remove(this._timeoutId);
            this._timeoutId = 0;
            this._reject(new Gio.IOErrorEnum({code: Gio.IOErrorEnum.CANCELLED, message: 'cancelled'}));
        }
        this.texture = null;
    }
}

// The same color state GNOME 51 gives a wallpaper.
function colorStateOf(frame) {
    const cicp = frame.get_color_cicp();
    if (!cicp)
        return null;
    try {
        return Clutter.ColorStateParams.new_from_cicp(global.stage.context, new Clutter.Cicp({
            primaries: cicp.color_primaries,
            transfer: cicp.transfer_characteristics,
            matrix_coefficients: cicp.matrix_coefficients,
            video_full_range_flag: cicp.video_full_range_flag,
        }));
    } catch (e) {
        console.warn(`Wallslæd: no color state from CICP: ${e.message}`);
        return null;
    }
}
