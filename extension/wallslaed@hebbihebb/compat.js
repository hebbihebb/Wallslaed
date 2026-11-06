// What differs between the GNOME Shell versions the extension supports (50
// and 51), detected by feature rather than version number. Everything else
// asks here instead of testing APIs itself.

import Cogl from 'gi://Cogl';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Glycin from 'gi://Gly';
import Meta from 'gi://Meta';
import Shell from 'gi://Shell';
import St from 'gi://St';

Gio._promisify(Glycin.Loader.prototype, 'load_async');
Gio._promisify(Glycin.Image.prototype, 'next_frame_async');

/** GNOME 51: backgrounds take a texture; 50 loads files through its own cache. */
export const TEXTURE_BACKGROUNDS = typeof Meta.Background.prototype.set_texture === 'function';

/** GNOME 50 still has Shell.GLSLEffect; 51 removed it for Clutter.ShaderEffect snippets. */
export const GLSL_EFFECT = typeof Shell.GLSLEffect === 'function';

/** GNOME 51 blurs behind app windows that ask for it, with a radius we can set. */
export const APP_BLUR = typeof global.compositor.get_background_blur_params === 'function';

/** The reduced-motion accessibility setting (GNOME 51; 50 doesn't have one). */
export function reducedMotion() {
    const motion = St.Settings.get().reduced_motion;
    return motion !== undefined && motion === St.ReducedMotion?.REDUCE;
}

/**
 * Loads `file` for a background without blocking the Shell. Resolves to a
 * function that makes a Meta.Background showing it in `style`, so the caller
 * can swap only once the picture is ready and nothing flashes black.
 */
export async function loadBackground(file, cancellable) {
    if (!TEXTURE_BACKGROUNDS)
        return loadThroughCache(file, cancellable);
    const {texture, colorState} = await loadTexture(file, cancellable);
    return style => {
        const background = new Meta.Background({meta_display: global.display});
        background.set_texture(texture, style, colorState);
        return background;
    };
}

/** Loads `file` into the existing Meta.Background `background`, in `style`. */
export async function loadInto(background, file, style, cancellable) {
    if (TEXTURE_BACKGROUNDS) {
        const {texture, colorState} = await loadTexture(file, cancellable);
        background.set_texture(texture, style, colorState);
    } else {
        await cached(file, cancellable);
        background.set_file(file, style);
    }
}

// GNOME 50: its background image cache decodes in a thread; once the image is
// in the cache, set_file shows it at once.
async function loadThroughCache(file, cancellable) {
    await cached(file, cancellable);
    return style => {
        const background = new Meta.Background({meta_display: global.display});
        background.set_file(file, style);
        return background;
    };
}

// GNOME 50: `file` in Meta.BackgroundImageCache, decoded.
function cached(file, cancellable) {
    const image = Meta.BackgroundImageCache.get_default().load(file);
    if (image.is_loaded())
        return image.get_success() ? Promise.resolve() : Promise.reject(new Error('GNOME could not load it'));
    return new Promise((resolve, reject) => {
        let settled = false;
        let cancelId = 0;
        const loadedId = image.connect('loaded', () => {
            if (settled)
                return;
            settled = true;
            image.disconnect(loadedId);
            // Not from inside the cancel handler, so this can't deadlock.
            if (cancelId)
                cancellable.disconnect(cancelId);
            if (image.get_success())
                resolve();
            else
                reject(new Error('GNOME could not load it'));
        });
        // Runs at once if already cancelled. A GCancellable handler must not
        // disconnect itself; the cancellable is dropped right after anyway.
        cancelId = cancellable?.connect(() => {
            if (settled)
                return;
            settled = true;
            image.disconnect(loadedId);
            reject(new Gio.IOErrorEnum({code: Gio.IOErrorEnum.CANCELLED, message: 'cancelled'}));
        }) ?? 0;
    });
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
