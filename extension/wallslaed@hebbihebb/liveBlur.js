// Live blur behind menus, quick settings, the calendar, notifications, OSDs,
// the window switcher, dialogs, app folders and the screenshot panel. These
// float over windows, so the baked wallpaper blur can't stand in; GNOME's own
// background blur does, but only while the popup is open: it is attached when
// the popup opens and gone when it closes, so nothing builds up.
//
// The blur sits in a sibling actor just below the popup, not inside it: menus
// draw themselves offscreen, where a background blur would see nothing.
//
// GNOME's background blur copies exactly its own rectangle of the screen and
// blurs that. When only part of a popup is redrawn (an item hovered), the rest
// of the rectangle still holds last frame's picture, popup and blur included,
// and blurring that left boxes that stayed until a full redraw. So:
// - while a popup is in use (pointer or keys inside it, and ACTIVE_US after),
//   every frame redraws its whole rectangle. A redraw queued at the stage's
//   before-update joins that same frame; it also schedules one more, which is
//   why this only runs for a moment and not all the time;
// - any other frame that redraws only part of a rectangle (a window changing
//   under the popup) has the whole of it redrawn on the next frame, and
//   starts the same moment.
//
// Over bare desktop none of that is needed: what is behind the popup is the
// wallpaper, and the daemon has already baked it blurred (blur.js). There the
// popup shows its piece of that texture instead, with no blur shader and no
// whole-rectangle redraws (60% less shell CPU while hovering a menu). Any
// window, other popup, the top bar, a transition or a video wallpaper under
// it, checked every frame, switches it back to the live blur.

import Clutter from 'gi://Clutter';
import GLib from 'gi://GLib';
import Meta from 'gi://Meta';
import Mtk from 'gi://Mtk';
import Shell from 'gi://Shell';
import St from 'gi://St';

import * as AppDisplay from 'resource:///org/gnome/shell/ui/appDisplay.js';
import * as BoxPointer from 'resource:///org/gnome/shell/ui/boxpointer.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as MessageTray from 'resource:///org/gnome/shell/ui/messageTray.js';
import * as ModalDialog from 'resource:///org/gnome/shell/ui/modalDialog.js';
import * as OsdWindow from 'resource:///org/gnome/shell/ui/osdWindow.js';
import * as Screenshot from 'resource:///org/gnome/shell/ui/screenshot.js';
import * as SwitcherPopup from 'resource:///org/gnome/shell/ui/switcherPopup.js';

const SURFACE_CLASS = 'wallslaed-blur-surface';
// Properties that move or fade a popup.
const TRACKED = ['allocation', 'translation-x', 'translation-y', 'scale-x', 'scale-y', 'opacity', 'visible'];
// Which setting each kind of surface follows.
const CATEGORY_KEYS = {popups: 'blur-popups', folders: 'blur-app-folders', screenshot: 'blur-screenshot'};
// How long after the last activity whole rectangles keep being redrawn: a bit
// longer than GNOME's hover and press transitions.
const ACTIVE_US = 300 * 1000;
// The brightness the stylesheet's tints are made for.
const STYLESHEET_BRIGHTNESS = 0.6;

export class LiveBlur {
    constructor(settings, injections, layers) {
        this._settings = settings;
        this._layers = layers; // BlurLayers, for the baked wallpaper blur
        this._blurs = new Map(); // target -> Blur
        this._signals = [];
        this._frameIds = []; // stage signals, while any blur is up
        this._activeUntil = 0; // monotonic µs
        this._destroyed = false;

        const self = this;
        // Run `attach` after the original method, or after its promise. A
        // promise can settle after this has been destroyed (on disable or
        // lock): then it does nothing.
        const after = (proto, name, attach) => {
            injections.overrideMethod(proto, name, original => function (...args) {
                const result = original.apply(this, args);
                const run = () => {
                    if (self._destroyed)
                        return;
                    try {
                        attach(this);
                    } catch (e) {
                        console.warn(`Wallslæd: popup blur: ${e.message}`);
                    }
                };
                if (typeof result?.then === 'function')
                    result.then(run, () => {});
                else
                    run();
                return result;
            });
        };
        const popups = () => this._settings.get_boolean('blur-popups');

        // Panel menus, quick settings, the calendar, the desktop menu and
        // context menus all open through a BoxPointer.
        after(BoxPointer.BoxPointer.prototype, 'open', box => {
            if (popups())
                self._attach('popups', box, box.bin.get_child() ?? box.bin);
        });
        after(ModalDialog.ModalDialog.prototype, 'open', dialog => {
            if (popups())
                self._attach('popups', dialog, dialog.dialogLayout._dialog ?? dialog.dialogLayout);
        });
        after(SwitcherPopup.SwitcherPopup.prototype, '_showImmediately', popup => {
            if (popups() && popup._switcherList)
                self._attach('popups', popup, popup._switcherList);
        });
        after(OsdWindow.OsdWindow.prototype, 'show', osd => {
            if (popups() && osd.visible && osd._hbox)
                self._attach('popups', osd, osd._hbox);
        });
        after(MessageTray.MessageTray.prototype, '_showNotification', tray => {
            if (popups() && tray._banner)
                self._attach('popups', tray._banner, tray._banner);
        });
        after(AppDisplay.AppFolderDialog.prototype, 'popup', dialog => {
            if (this._settings.get_boolean('blur-app-folders') && dialog._isOpen)
                self._attach('folders', dialog, dialog._viewBox);
        });
        // Opening is async; a close in between leaves it invisible.
        after(Screenshot.ScreenshotUI.prototype, 'open', ui => {
            if (this._settings.get_boolean('blur-screenshot') && ui._panel && ui.visible)
                self._attach('screenshot', childOf(ui, ui._panel), ui._panel);
        });
        // The screenshot UI hides itself, not the panel's container.
        after(Screenshot.ScreenshotUI.prototype, 'close', ui => {
            if (ui._panel)
                self._blurs.get(ui._panel)?.destroy();
        });

        // Open popups follow the look settings at once. A kind switched off
        // loses its blur now; switched on, it applies from the next popup.
        this._signals.push([settings, settings.connect('changed', (_s, key) => {
            if (key === 'blur-strength' || key === 'blur-brightness') {
                this._blurs.forEach(blur => blur.setLook(this._look()));
                return;
            }
            for (const blur of [...this._blurs.values()]) {
                if (CATEGORY_KEYS[blur.category] === key && !settings.get_boolean(key))
                    blur.destroy();
            }
        })]);
    }

    _look() {
        return {
            radius: this._settings.get_uint('blur-strength') * 2,
            brightness: this._settings.get_double('blur-brightness'),
        };
    }

    // See the comment at the top.
    _syncFrameHook() {
        const stage = global.stage;
        if (this._blurs.size && !this._frameIds.length) {
            this._frameIds = [
                stage.connect('before-update', () => {
                    for (const blur of this._blurs.values())
                        blur.syncSource(this._baked(blur));
                    if (GLib.get_monotonic_time() < this._activeUntil)
                        this._redrawAll();
                }),
                // A null region means the whole view.
                stage.connect('paint-view', (_stage, _view, region) => {
                    if (region && [...this._blurs.values()].some(blur => blur.partlyIn(region))) {
                        this._redrawAll();
                        this._activate();
                    }
                }),
            ];
        } else if (!this._blurs.size && this._frameIds.length) {
            this._frameIds.forEach(id => stage.disconnect(id));
            this._frameIds = [];
            this._activeUntil = 0;
        }
    }

    // The baked wallpaper blur for `blur`'s rectangle if only the wallpaper is
    // behind it, else null.
    _baked(blur) {
        const rect = blur.rect();
        if (!rect || Main.overview.visible || Main.layoutManager.screenTransition.visible)
            return null;
        const index = Main.layoutManager.monitors.findIndex(m => within(rect, m));
        const content = index < 0 ? null : this._layers.bakedContent(index);
        const manager = Main.layoutManager._bgManagers[index];
        // A transition (an outgoing background) on it.
        if (!content || !manager ||
            Main.layoutManager._backgroundGroup.get_children().filter(a => a.visible).length >
                Main.layoutManager._bgManagers.length)
            return null;
        // A video (clones), or anything else drawn on the wallpaper under it
        // (Fedora's logo, in a monitor-sized holder), isn't in the baked blur.
        for (const child of manager.backgroundActor.get_children()) {
            if (child instanceof Clutter.Clone || drawnParts(child).some(a => overlaps(rect, extentsOf(a))))
                return null;
        }
        if (Main.panel.visible && overlaps(rect, extentsOf(Main.panel)))
            return null;
        for (const actor of global.get_window_actors()) {
            if (actor.mapped && actor.get_paint_opacity() > 0 && overlaps(rect, extentsOf(actor)))
                return null;
        }
        for (const other of this._blurs.values()) {
            const r = other !== blur ? other.rect() : null;
            if (r && overlaps(rect, r))
                return null;
        }
        return {content, monitor: Main.layoutManager.monitors[index]};
    }

    _activate() {
        this._activeUntil = GLib.get_monotonic_time() + ACTIVE_US;
    }

    _redrawAll() {
        for (const blur of this._blurs.values())
            blur.redraw();
    }

    /**
     * Blur behind `target` while `root` (the popup, or the child of the
     * popup's container that holds `target`) stays visible. The blur goes
     * just below `root` in its parent.
     */
    _attach(category, root, target) {
        if (this._blurs.has(target) || !root.get_parent())
            return;
        const blur = new Blur(category, root, target, this._look(), () => {
            this._blurs.delete(target);
            this._syncFrameHook();
        }, () => {
            this._activate();
            this._redrawAll();
        });
        this._blurs.set(target, blur);
        this._syncFrameHook();
    }

    destroy() {
        this._destroyed = true;
        for (const [obj, id] of this._signals)
            obj.disconnect(id);
        this._signals = [];
        for (const blur of [...this._blurs.values()])
            blur.destroy();
        this._blurs.clear();
        this._syncFrameHook();
    }
}

function extentsOf(actor) {
    const box = actor.get_transformed_extents();
    return {x: box.get_x(), y: box.get_y(), width: box.get_width(), height: box.get_height()};
}

// The visible actors without children under `actor`: what it actually draws.
function drawnParts(actor) {
    if (!actor.visible)
        return [];
    const children = actor.get_children();
    return children.length ? children.flatMap(drawnParts) : [actor];
}

function overlaps(a, b) {
    return a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height;
}

function within(a, b) {
    return a.x >= b.x && a.y >= b.y && a.x + a.width <= b.x + b.width && a.y + a.height <= b.y + b.height;
}

// The child of `container` that holds `actor`.
function childOf(container, actor) {
    let a = actor;
    while (a.get_parent() && a.get_parent() !== container)
        a = a.get_parent();
    return a;
}

class Blur {
    constructor(category, root, target, look, onDestroyed, onActivity) {
        this.category = category;
        this._root = root;
        this._target = target;
        this._onDestroyed = onDestroyed;
        this._signals = [];
        this._destroyed = false;
        this._laterId = 0;

        const parent = root.get_parent();
        // At the parent's origin whatever its layout: fixed layouts leave it
        // at 0,0, box and bin layouts stretch it over the whole parent.
        this._group = new Clutter.Actor({
            name: 'wallslaed-popup-blur',
            reactive: false,
            x_expand: true,
            y_expand: true,
        });
        // One rectangle, the popup's full size. Its square corners reach a
        // little past the rounded ones, under the popup's shadow; smaller or
        // stacked rectangles leave sharp bands or blur each other's output.
        // An St.Widget: a bare actor has no paint volume, and the blur needs
        // one to know what to cover.
        this._part = new St.Widget({reactive: false});
        // Full brightness: the popup's tint darkens, inside its rounded corners.
        this._effect = new Shell.BlurEffect({mode: Shell.BlurMode.BACKGROUND, radius: look.radius, brightness: 1});
        this._part.add_effect(this._effect);
        this._group.add_child(this._part);
        // Over bare desktop: the baked blur, clipped to the same rectangle.
        this._baked = null; // {content, monitor} shown, or null for live
        this._bakedClip = new Clutter.Actor({clip_to_allocation: true, visible: false});
        this._bakedPicture = new Clutter.Actor();
        this._bakedClip.add_child(this._bakedPicture);
        this._group.add_child(this._bakedClip);
        parent.insert_child_below(this._group, root);
        target.add_style_class_name(SURFACE_CLASS);
        this._style = target.style;
        this._tint = target.get_theme_node().get_background_color();
        this._look = look;
        this.setLook(look);

        // Watch everything between the popup and the target: any of them can
        // move, scale or fade it.
        for (let a = target; a; a = a.get_parent()) {
            for (const prop of TRACKED)
                this._signals.push([a, a.connect(`notify::${prop}`, () => this._queueUpdate())]);
            if (a === root)
                break;
        }
        // Pointer and key events inside the popup come before the hover and
        // press changes they cause.
        this._signals.push([root, root.connect('captured-event', () => {
            onActivity();
            return Clutter.EVENT_PROPAGATE;
        })]);
        this._signals.push([root, root.connect('destroy', () => this.destroy())]);
        this._signals.push([target, target.connect('destroy', () => {
            this._target = null;
            this.destroy();
        })]);
        this._update();
    }

    // Once per frame at most, and never in the middle of a relayout.
    _queueUpdate() {
        if (this._laterId || this._destroyed)
            return;
        this._laterId = global.compositor.get_laters().add(Meta.LaterType.BEFORE_REDRAW, () => {
            this._laterId = 0;
            this._update();
            return false;
        });
    }

    // The tint's transparency scales with the brightness setting. The baked
    // blur has the brightness in it already: the tint leaves out that part.
    setLook(look) {
        this._look = look;
        const {radius, brightness} = look;
        this._effect.radius = radius;
        const {red, green, blue, alpha} = this._tint;
        const clear = (1 - alpha / 255) * (this._baked ? 1 : brightness) / STYLESHEET_BRIGHTNESS;
        const a = Math.min(1, Math.max(0, 1 - clear)).toFixed(3);
        this._target.style = `${this._style ?? ''} background-color: rgba(${red}, ${green}, ${blue}, ${a});`;
    }

    /** The blurred rectangle in stage coordinates, or null while it isn't shown. */
    rect() {
        if (this._destroyed || !this._group.mapped)
            return null;
        return extentsOf(this._part);
    }

    // Show `baked` ({content, monitor}, see LiveBlur._baked) or, if null, the
    // live blur.
    syncSource(baked) {
        if (this._destroyed || baked?.content === this._baked?.content)
            return;
        const wasBaked = !!this._baked;
        this._baked = baked;
        this._effect.enabled = !baked;
        this._bakedClip.visible = !!baked;
        if (baked)
            this._bakedPicture.set({content: baked.content, width: baked.monitor.width, height: baked.monitor.height});
        else
            this._bakedPicture.content = null;
        this._placeBaked();
        if (wasBaked !== !!baked)
            this.setLook(this._look);
    }

    // The monitor-sized picture, positioned so its piece under the popup shows.
    _placeBaked() {
        if (!this._baked)
            return;
        this._bakedClip.set_position(this._part.x, this._part.y);
        this._bakedClip.set_size(this._part.width, this._part.height);
        const [x, y] = this._part.get_transformed_position();
        this._bakedPicture.set_position(Math.round(this._baked.monitor.x - x), Math.round(this._baked.monitor.y - y));
    }

    redraw() {
        if (!this._destroyed && !this._baked && this._part.mapped)
            this._part.queue_redraw();
    }

    // Whether `region`, the stage area a frame redraws, covers only part of
    // the blurred rectangle.
    partlyIn(region) {
        if (this._destroyed || this._baked || !this._part.mapped)
            return false;
        // A pixel inside each edge, so rounding never makes a full redraw
        // look partial.
        const box = this._part.get_transformed_extents();
        const x = Math.ceil(box.get_x()) + 1, y = Math.ceil(box.get_y()) + 1;
        const rect = new Mtk.Rectangle({
            x, y,
            width: Math.floor(box.get_x() + box.get_width()) - 1 - x,
            height: Math.floor(box.get_y() + box.get_height()) - 1 - y,
        });
        return rect.width > 0 && rect.height > 0 &&
            region.contains_rectangle(rect) === Mtk.RegionOverlap.PART;
    }

    _update() {
        if (this._destroyed)
            return;
        // Gone once the popup has finished closing.
        if (!this._root.visible || !this._target.get_parent()) {
            this.destroy();
            return;
        }

        let opacity = 1;
        for (let a = this._target; a; a = a.get_parent()) {
            opacity *= a.opacity / 255;
            if (a === this._root)
                break;
        }
        this._group.opacity = Math.round(255 * opacity);

        // The group sits at its parent's origin, so parent coordinates work
        // for its children too. Popup containers aren't scaled or rotated,
        // so an offset is enough (and transform_stage_point() fails on the
        // zero-size containers some menus use).
        const [px, py] = this._group.get_parent().get_transformed_position();
        const extents = this._target.get_transformed_extents();
        const x1 = extents.get_x() - px, y1 = extents.get_y() - py;
        const x2 = x1 + extents.get_width(), y2 = y1 + extents.get_height();

        const [w, h] = [x2 - x1, y2 - y1];
        // Not laid out yet on the first frame.
        if (![x1, y1, w, h].every(Number.isFinite) || w <= 0 || h <= 0) {
            this._group.hide();
            return;
        }
        this._group.show();
        this._part.set_position(Math.round(x1), Math.round(y1));
        this._part.set_size(Math.round(w), Math.round(h));
        this._placeBaked();
    }

    destroy() {
        if (this._destroyed)
            return;
        this._destroyed = true;
        if (this._laterId)
            global.compositor.get_laters().remove(this._laterId);
        this._laterId = 0;
        for (const [obj, id] of this._signals)
            obj.disconnect(id);
        this._signals = [];
        if (this._target) {
            this._target.style = this._style;
            this._target.remove_style_class_name(SURFACE_CLASS);
        }
        this._group.destroy();
        this._onDestroyed();
    }
}
