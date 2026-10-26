// Loads wallpapers without stalling the compositor. GNOME 51 decodes a new
// wallpaper synchronously and uploads it to the GPU in one call, on the
// compositor's own thread: 125-155 ms at 4K, even with transitions off. The
// daemon announces the pictures Next and Previous would show (`next-picture`,
// `previous-picture`); they are decoded here through glycin's async loaders and
// uploaded a few rows at a time while the shell is idle. When GNOME then loads
// one, it gets the finished texture. Any other picture is loaded the same way
// on demand, so it shows up a moment later instead of freezing the screen.
//
// Everything else stays GNOME's: the settings, its Background objects, the
// actor swap the transitions hook into. Only the first wallpaper after login
// (loaded before the extension starts) and pictures this fails to load go
// through GNOME's own loader.
//
// Textures loaded here never enter GNOME's texture cache, which keeps every
// wallpaper it has loaded on the GPU for the rest of the session (2 GB after
// 64 4K wallpapers); this keeps only the current, next and previous ones.

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

// The memory formats GNOME 51 accepts from glycin, and their Cogl formats.
const M = Glycin.MemoryFormat;
const S = Glycin.MemoryFormatSelection;
const P = Cogl.PixelFormat;
const FORMATS = [
    [M.B8G8R8A8_PREMULTIPLIED, S.B8G8R8A8_PREMULTIPLIED, P.BGRA_8888_PRE],
    [M.A8R8G8B8_PREMULTIPLIED, S.A8R8G8B8_PREMULTIPLIED, P.ARGB_8888_PRE],
    [M.R8G8B8A8_PREMULTIPLIED, S.R8G8B8A8_PREMULTIPLIED, P.RGBA_8888_PRE],
    [M.B8G8R8A8, S.B8G8R8A8, P.BGRA_8888],
    [M.A8R8G8B8, S.A8R8G8B8, P.ARGB_8888],
    [M.R8G8B8A8, S.R8G8B8A8, P.RGBA_8888],
    [M.A8B8G8R8, S.A8B8G8R8, P.ABGR_8888],
    [M.R8G8B8, S.R8G8B8, P.RGB_888],
    [M.B8G8R8, S.B8G8R8, P.BGR_888],
    [M.R16G16B16A16_PREMULTIPLIED, S.R16G16B16A16_PREMULTIPLIED, P.RGBA_16161616_PRE],
    [M.R16G16B16A16, S.R16G16B16A16, P.RGBA_16161616],
    [M.R16G16B16A16_FLOAT, S.R16G16B16A16_FLOAT, P.RGBA_FP_16161616],
    [M.R32G32B32A32_FLOAT_PREMULTIPLIED, S.R32G32B32A32_FLOAT_PREMULTIPLIED, P.RGBA_FP_32323232_PRE],
    [M.R32G32B32A32_FLOAT, S.R32G32B32A32_FLOAT, P.RGBA_FP_32323232],
];
const ACCEPTED = FORMATS.reduce((all, [, selection]) => all | selection, 0);
const COGL_FORMATS = new Map(FORMATS.map(([memory, , cogl]) => [memory, cogl]));

const now = () => GLib.get_monotonic_time() / 1000;

export class Preloader {
    constructor(daemon, injections, busy) {
        this._daemon = daemon;
        this._injections = injections;
        // True while a transition runs; uploads wait for it to end.
        this._busy = busy;
        // uri -> Entry: the next and previous pictures, the current one once
        // shown, and pictures GNOME asked for that are still loading.
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
        const uri = file.get_uri();
        let entry = this._entries.get(uri);
        if (!entry) {
            // Not prepared: load it now, without waiting for transitions.
            entry = new Entry(file, () => false);
            entry.demanded = true;
            this._entries.set(uri, entry);
        }
        try {
            await entry.ready;
            // Rewritten since it was prepared (GNOME reloads on file changes).
            const info = await file.query_info_async(Gio.FILE_ATTRIBUTE_ETAG_VALUE,
                Gio.FileQueryInfoFlags.NONE, GLib.PRIORITY_DEFAULT, null);
            if (info.get_etag() !== entry.etag)
                throw new Error('changed on disk');
            if (!entry.texture)
                return null;
            // GNOME's Background holds the texture from here; keep it only
            // if the daemon still has a use for it.
            entry.demanded = false;
            if (!this._wanted().has(uri))
                this._entries.delete(uri);
            return entry;
        } catch (e) {
            if (!e.matches?.(Gio.IOErrorEnum, Gio.IOErrorEnum.CANCELLED))
                console.warn(`Wallslæd: not preloaded ${file.get_path()}: ${e.message}`);
            if (this._entries.get(uri) === entry)
                this._drop(uri);
            return null;
        }
    }

    // The pictures on screen or one step away.
    _wanted() {
        const state = this._daemon.state;
        return new Set([state?.picture, state?.['next-picture'], state?.['previous-picture']]
            .filter(path => path)
            .map(path => Gio.File.new_for_path(path).get_uri()));
    }

    // Keep the current, next and previous pictures; prepare the next and
    // previous ones.
    _sync() {
        const state = this._daemon.state;
        const wanted = this._wanted();
        for (const [key, entry] of [...this._entries]) {
            if (!wanted.has(key) && !entry.demanded)
                this._drop(key);
        }
        for (const path of [state?.['next-picture'], state?.['previous-picture']]) {
            const file = path ? Gio.File.new_for_path(path) : null;
            if (file && !this._entries.has(file.get_uri()))
                this._entries.set(file.get_uri(), new Entry(file, this._busy));
        }
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
        // Asked for by GNOME and still loading: kept until it is handed over.
        this.demanded = false;
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

        const memoryFormat = frame.get_memory_format();
        const coglFormat = COGL_FORMATS.get(memoryFormat);
        if (coglFormat === undefined)
            throw new Error(`unexpected memory format ${memoryFormat}`);
        const colorState = colorStateOf(frame);

        // As GNOME 51 makes them, including the sliced fallback for pictures
        // too large for one texture.
        const width = frame.get_width();
        const height = frame.get_height();
        const components = Glycin.memory_format_has_alpha(memoryFormat)
            ? Cogl.TextureComponents.RGBA : Cogl.TextureComponents.RGB;
        const context = global.stage.context.get_backend().get_cogl_context();
        let texture = Cogl.Texture2D.new_with_size(context, width, height);
        texture.set_components(components);
        try {
            texture.allocate();
        } catch {
            texture = Cogl.Texture2DSliced.new_with_size(context, width, height, Cogl.TEXTURE_MAX_WASTE);
            texture.set_components(components);
        }

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
