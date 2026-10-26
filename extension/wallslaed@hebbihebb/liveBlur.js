// Live blur behind menus, quick settings, the calendar, notifications, OSDs,
// the window switcher, dialogs, app folders and the screenshot panel. These
// float over windows, so the baked wallpaper blur can't stand in; GNOME's own
// background blur does, but only while the popup is open: it is attached when
// the popup opens and gone when it closes, so nothing builds up.
//
// The blur sits in a sibling actor just below the popup, not inside it: menus
// draw themselves offscreen, where a background blur would see nothing.

import Clutter from 'gi://Clutter';
import Meta from 'gi://Meta';
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
// GNOME's blur is rectangular. These rectangles, given as insets in units of
// the corner radius, make an octagon that fits inside the rounded corners and
// leaves at most ~8% of the radius unblurred there.
const SHAPE = [[1, 0], [0, 1], [1 - Math.SQRT1_2, 1 - Math.SQRT1_2]];

export class LiveBlur {
    constructor(settings, injections) {
        this._settings = settings;
        this._blurs = new Map(); // target -> Blur
        this._signals = [];

        const self = this;
        // Run `attach` after the original method, or after its promise.
        const after = (proto, name, attach) => {
            injections.overrideMethod(proto, name, original => function (...args) {
                const result = original.apply(this, args);
                const run = () => {
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
                self._attach(box, box.bin.get_child() ?? box.bin);
        });
        after(ModalDialog.ModalDialog.prototype, 'open', dialog => {
            if (popups())
                self._attach(dialog, dialog.dialogLayout._dialog ?? dialog.dialogLayout);
        });
        after(SwitcherPopup.SwitcherPopup.prototype, '_showImmediately', popup => {
            if (popups() && popup._switcherList)
                self._attach(popup, popup._switcherList);
        });
        after(OsdWindow.OsdWindow.prototype, 'show', osd => {
            if (popups() && osd.visible && osd._hbox)
                self._attach(osd, osd._hbox);
        });
        after(MessageTray.MessageTray.prototype, '_showNotification', tray => {
            if (popups() && tray._banner)
                self._attach(tray._banner, tray._banner);
        });
        after(AppDisplay.AppFolderDialog.prototype, 'popup', dialog => {
            if (this._settings.get_boolean('blur-app-folders') && dialog._isOpen)
                self._attach(dialog, dialog._viewBox);
        });
        after(Screenshot.ScreenshotUI.prototype, 'open', ui => {
            if (this._settings.get_boolean('blur-screenshot') && ui._panel && ui.visible)
                self._attach(childOf(ui, ui._panel), ui._panel);
        });
        // The screenshot UI hides itself, not the panel's container.
        after(Screenshot.ScreenshotUI.prototype, 'close', ui => {
            if (ui._panel)
                self._blurs.get(ui._panel)?.destroy();
        });

        this._signals.push([settings, settings.connect('changed', (_s, key) => {
            if (key === 'blur-strength')
                this._blurs.forEach(blur => blur.setRadius(this._radius()));
            else if (key.startsWith('blur-'))
                this._clear(); // Popups opened from now on follow the new setting.
        })]);
    }

    _radius() {
        return this._settings.get_uint('blur-strength') * 2;
    }

    /**
     * Blur behind `target` while `root` (the popup, or the child of the
     * popup's container that holds `target`) stays visible. The blur goes
     * just below `root` in its parent.
     */
    _attach(root, target) {
        if (this._blurs.has(target) || !root.get_parent())
            return;
        const blur = new Blur(root, target, this._radius(), () => this._blurs.delete(target));
        this._blurs.set(target, blur);
    }

    _clear() {
        for (const blur of [...this._blurs.values()])
            blur.destroy();
        this._blurs.clear();
    }

    destroy() {
        for (const [obj, id] of this._signals)
            obj.disconnect(id);
        this._signals = [];
        this._clear();
    }
}

// The child of `container` that holds `actor`.
function childOf(container, actor) {
    let a = actor;
    while (a.get_parent() && a.get_parent() !== container)
        a = a.get_parent();
    return a;
}

class Blur {
    constructor(root, target, radius, onDestroyed) {
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
        this._parts = SHAPE.map(() => {
            // An St.Widget: a bare actor has no paint volume, and the blur
            // needs one to know what to cover.
            const part = new St.Widget({reactive: false});
            part.add_effect_with_name('blur', new Shell.BlurEffect({
                mode: Shell.BlurMode.BACKGROUND,
                radius,
                // Overlapping parts would darken twice; the popup's own
                // background tints instead.
                brightness: 1,
            }));
            this._group.add_child(part);
            return part;
        });
        parent.insert_child_below(this._group, root);
        target.add_style_class_name(SURFACE_CLASS);

        // Watch everything between the popup and the target: any of them can
        // move, scale or fade it.
        for (let a = target; a; a = a.get_parent()) {
            for (const prop of TRACKED)
                this._signals.push([a, a.connect(`notify::${prop}`, () => this._queueUpdate())]);
            if (a === root)
                break;
        }
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

    setRadius(radius) {
        for (const part of this._parts)
            part.get_effect('blur').radius = radius;
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
        const scale = this._target.width > 0 ? w / this._target.width : 1;
        const node = this._target.get_theme_node();
        const r = Math.min(node.get_border_radius(St.Corner.TOPLEFT) * scale, w / 2, h / 2);
        // Not laid out yet on the first frame.
        if (![x1, y1, w, h, r].every(Number.isFinite) || w <= 0 || h <= 0) {
            this._group.hide();
            return;
        }
        this._group.show();
        this._parts.forEach((part, i) => {
            const [ix, iy] = SHAPE[i].map(k => Math.ceil(k * r));
            part.set_position(Math.round(x1) + ix, Math.round(y1) + iy);
            part.set_size(Math.max(0, Math.round(w) - 2 * ix), Math.max(0, Math.round(h) - 2 * iy));
        });
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
        this._target?.remove_style_class_name(SURFACE_CLASS);
        this._group.destroy();
        this._onDestroyed();
    }
}
