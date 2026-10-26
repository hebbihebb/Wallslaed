// Blur behind the overview, the top bar and the dash. The daemon bakes a small,
// blurred and darkened copy of each wallpaper (the `blurred` state entry), so
// all that happens here is showing that tiny image scaled up: one texture per
// monitor, shared by every surface, and no blur shader running. Only when
// there is no baked copy (a format the daemon can't read, or no daemon) does
// a GPU blur stand in, cached by Clutter until the next wallpaper. It blurs
// GNOME's own background object, so the full-size picture isn't loaded twice.

import Clutter from 'gi://Clutter';
import Cogl from 'gi://Cogl';
import GDesktopEnums from 'gi://GDesktopEnums';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Glycin from 'gi://Gly';
import Meta from 'gi://Meta';
import Shell from 'gi://Shell';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';

const FADE_MS = 300;

Gio._promisify(Glycin.Loader.prototype, 'load_async');
Gio._promisify(Glycin.Image.prototype, 'next_frame_async');

// What the extension adds to GNOME's own styles, scoped by these classes.
const OVERVIEW_CLASS = 'wallslaed-blur-overview';
const PANEL_CLASS = 'wallslaed-blur-panel';
const DASH_CLASS = 'wallslaed-blur-dash';

export class BlurLayers {
    constructor(settings, daemon) {
        this._settings = settings;
        this._daemon = daemon;
        this._background = new Gio.Settings({schema_id: 'org.gnome.desktop.background'});
        this._source = null; // {file, gpu}
        this._cancellable = null; // a baked copy being loaded
        this._gnomeBackground = null; // GNOME's background the GPU blur shows
        this._gnomeIds = [];
        this._reloadId = 0;
        // Per monitor: {content, gpu}. Surfaces show these.
        this._contents = [];
        this._overview = null; // {group, layers: [Layer]}
        this._panel = null; // {wrapper, layer}
        this._signals = [];

        const on = (obj, signal, fn) => this._signals.push([obj, obj.connect(signal, fn)]);
        on(daemon, 'changed', () => this._load());
        on(this._background, 'changed::picture-uri', () => this._load());
        on(this._background, 'changed::picture-options', () => this._load(true));
        on(settings, 'changed', (_s, key) => {
            if (key === 'blur-strength' || key === 'blur-brightness') {
                // The daemon re-bakes and reports a new file; only a GPU
                // stand-in has to follow the settings here.
                if (this._source?.gpu)
                    this._load(true);
            } else if (key === 'blur-overview' || key === 'blur-panel' || key === 'blur-dash') {
                this._sync();
            }
        });
        on(Main.layoutManager, 'monitors-changed', () => this._rebuild());

        this._sync();
    }

    get _wanted() {
        return this._settings.get_boolean('blur-overview') || this._settings.get_boolean('blur-panel');
    }

    _sync() {
        Main.uiGroup.remove_style_class_name(DASH_CLASS);
        if (this._settings.get_boolean('blur-dash') && this._settings.get_boolean('blur-overview'))
            Main.uiGroup.add_style_class_name(DASH_CLASS);
        this._rebuild();
    }

    _rebuild() {
        this._destroySurfaces();
        if (!this._wanted) {
            this._clearSource();
            return;
        }
        if (this._settings.get_boolean('blur-overview'))
            this._createOverview();
        if (this._settings.get_boolean('blur-panel'))
            this._createPanel();
        this._load(true);
    }

    // Which file to show, and whether it still needs blurring on the GPU.
    _pickSource() {
        const state = this._daemon.state;
        if (state?.blurred)
            return {file: Gio.File.new_for_path(state.blurred), gpu: false};
        const uri = this._background.get_string('picture-uri');
        return state?.picture || uri ? {file: null, gpu: true, picture: state?.picture ?? uri} : null;
    }

    _load(force = false) {
        if (!this._wanted)
            return;
        const source = this._pickSource();
        if (!force && source?.gpu === this._source?.gpu && source?.picture === this._source?.picture &&
            (source?.file?.equal(this._source.file) ?? !this._source?.file))
            return;
        this._source = source;
        this._cancelLoad();
        // Nothing to show yet: keep whatever is up.
        if (!source)
            return;
        if (source.gpu)
            this._loadGnomeBackground();
        else
            this._loadBaked(source);
    }

    // Wait for the image before swapping, so nothing flashes black.
    async _loadBaked(source) {
        const cancellable = this._cancellable = new Gio.Cancellable();
        let loaded;
        try {
            loaded = await loadTexture(source.file, cancellable);
        } catch (e) {
            if (!e.matches?.(Gio.IOErrorEnum, Gio.IOErrorEnum.CANCELLED))
                console.warn(`Wallslæd: can't load ${source.file.get_path()}: ${e.message}`);
            return;
        }
        if (cancellable.is_cancelled())
            return;
        this._cancellable = null;
        const style = this._style();
        this._apply(() => {
            const background = new Meta.Background({meta_display: global.display});
            background.set_texture(loaded.texture, style, loaded.colorState);
            return background;
        });
    }

    // GNOME's own background object for the picture on screen, once loaded.
    // GNOME replaces it on every change ('bg-changed'), so follow that.
    _loadGnomeBackground() {
        const backgroundSource = Main.layoutManager._bgManagers[0]?._backgroundSource;
        const background = backgroundSource?.getBackground(0);
        if (!background)
            return;
        this._gnomeBackground = background;
        const on = (signal, fn) => this._gnomeIds.push(background.connect(signal, fn));
        on('bg-changed', () => {
            this._disconnectGnomeBackground();
            // After GNOME has dropped it from its cache.
            this._reloadId = GLib.idle_add(GLib.PRIORITY_DEFAULT, () => {
                this._reloadId = 0;
                this._load(true);
                return GLib.SOURCE_REMOVE;
            });
        });
        const apply = () => this._apply(() => background);
        if (background.isLoaded)
            apply();
        else
            on('loaded', apply);
    }

    _style() {
        const style = this._background.get_enum('picture-options');
        return style === GDesktopEnums.BackgroundStyle.NONE ? GDesktopEnums.BackgroundStyle.ZOOM : style;
    }

    _apply(makeBackground) {
        this._contents = Main.layoutManager.monitors.map((_m, index) => {
            const content = Meta.BackgroundContent.new(global.display, index);
            content.background = makeBackground();
            return {content, gpu: this._source.gpu};
        });
        this._overview?.layers.forEach(layer => layer.show(this._contents[layer.index]));
        this._panel?.layer.show(this._contents[this._panel.layer.index]);
    }

    _disconnectGnomeBackground() {
        for (const id of this._gnomeIds)
            this._gnomeBackground.disconnect(id);
        this._gnomeIds = [];
        this._gnomeBackground = null;
    }

    _cancelLoad() {
        this._cancellable?.cancel();
        this._cancellable = null;
        if (this._gnomeBackground)
            this._disconnectGnomeBackground();
        if (this._reloadId)
            GLib.source_remove(this._reloadId);
        this._reloadId = 0;
    }

    _effect() {
        return new Shell.BlurEffect({
            mode: Shell.BlurMode.ACTOR,
            radius: this._settings.get_uint('blur-strength') * 2,
            brightness: this._settings.get_double('blur-brightness'),
        });
    }

    _createOverview() {
        const group = new Clutter.Actor({name: 'wallslaed-overview-blur'});
        const layers = Main.layoutManager.monitors.map((monitor, index) => {
            const layer = new Layer(index, monitor, () => this._effect());
            layer.actor.set_position(monitor.x, monitor.y);
            group.add_child(layer.actor);
            return layer;
        });
        const overviewGroup = Main.layoutManager.overviewGroup;
        overviewGroup.insert_child_at_index(group, 0);
        // Stay at the bottom when the overview adds its own children.
        const id = overviewGroup.connect('child-added', (_g, child) => {
            if (child !== group)
                overviewGroup.set_child_below_sibling(group, null);
        });
        Main.uiGroup.add_style_class_name(OVERVIEW_CLASS);
        this._overview = {group, layers, id};
    }

    _createPanel() {
        const monitor = Main.layoutManager.primaryMonitor;
        if (!monitor)
            return;
        const panelBox = Main.layoutManager.panelBox;
        // A zero-size holder, so the panel box's layout gives it no room; the
        // monitor-sized layer inside paints past it, clipped to the panel.
        const wrapper = new Clutter.Actor({name: 'wallslaed-panel-blur', width: 0, height: 0});
        const layer = new Layer(monitor.index, monitor, () => this._effect());
        wrapper.add_child(layer.actor);
        panelBox.insert_child_at_index(wrapper, 0);
        const place = () => {
            if (!layer.actor)
                return;
            // The panel box sits at the monitor's top-left corner.
            const [x, y] = [panelBox.x - monitor.x, panelBox.y - monitor.y];
            layer.actor.set_position(-x, -y);
            layer.actor.set_clip(x, y, panelBox.width, Main.panel.height);
        };
        const ids = [
            [panelBox, panelBox.connect('notify::allocation', place)],
            [Main.panel, Main.panel.connect('notify::height', place)],
        ];
        place();
        Main.panel.add_style_class_name(PANEL_CLASS);
        this._panel = {wrapper, layer, ids};
    }

    _destroySurfaces() {
        if (this._overview) {
            Main.layoutManager.overviewGroup.disconnect(this._overview.id);
            this._overview.group.destroy();
            this._overview = null;
        }
        Main.uiGroup.remove_style_class_name(OVERVIEW_CLASS);
        if (this._panel) {
            for (const [obj, id] of this._panel.ids)
                obj.disconnect(id);
            this._panel.wrapper.destroy();
            this._panel = null;
        }
        Main.panel.remove_style_class_name(PANEL_CLASS);
    }

    _clearSource() {
        this._cancelLoad();
        this._source = null;
        this._contents = [];
    }

    destroy() {
        for (const [obj, id] of this._signals)
            obj.disconnect(id);
        this._signals = [];
        this._destroySurfaces();
        this._clearSource();
        Main.uiGroup.remove_style_class_name(DASH_CLASS);
        this._background = null;
    }
}

// One monitor's blurred wallpaper on one surface. New wallpapers fade in on
// top of the old one, which goes once covered.
class Layer {
    constructor(index, monitor, makeEffect) {
        this.index = index;
        this._makeEffect = makeEffect;
        this.actor = new Clutter.Actor({width: monitor.width, height: monitor.height});
        // GNOME may destroy it first when the shell shuts down.
        this.actor.connect('destroy', () => (this.actor = null));
        this._current = null;
    }

    show(entry) {
        if (!entry || !this.actor)
            return;
        const child = new Clutter.Actor({
            content: entry.content,
            width: this.actor.width,
            height: this.actor.height,
        });
        if (entry.gpu)
            child.add_effect(this._makeEffect());
        this.actor.add_child(child);

        const fade = !!this._current && this.actor.mapped;
        this._current = child;
        if (!fade) {
            this._dropBelow(child);
            return;
        }
        child.opacity = 0;
        child.ease({
            opacity: 255,
            duration: FADE_MS,
            mode: Clutter.AnimationMode.EASE_OUT_QUAD,
            onStopped: () => this._dropBelow(child),
        });
    }

    // Everything under `child` is hidden by it.
    _dropBelow(child) {
        for (const c of this.actor?.get_children() ?? []) {
            if (c === child)
                break;
            c.destroy();
        }
    }
}

// Loads an image into a texture the way GNOME 51's background cache does,
// but without blocking: through glycin's sandboxed loaders.
async function loadTexture(file, cancellable) {
    const stream = await file.read_async(GLib.PRIORITY_DEFAULT, cancellable);
    const loader = Glycin.Loader.new_for_stream(stream);
    loader.set_accepted_memory_formats(Glycin.MemoryFormatSelection.R8G8B8);
    const image = await loader.load_async(cancellable);
    const frame = await image.next_frame_async(cancellable);
    if (frame.get_memory_format() !== Glycin.MemoryFormat.R8G8B8)
        throw new Error(`unexpected memory format ${frame.get_memory_format()}`);

    const context = global.stage.context.get_backend().get_cogl_context();
    const texture = Cogl.Texture2D.new_with_size(context, frame.get_width(), frame.get_height());
    texture.set_components(Cogl.TextureComponents.RGB);
    if (!texture.set_data(Cogl.PixelFormat.RGB_888, frame.get_stride(), frame.get_buf_bytes().get_data(), 0))
        throw new Error('failed to set texture data');
    return {texture, colorState: null};
}
