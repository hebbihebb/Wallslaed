// Hides the top bar while a window of the focused app overlaps it. It comes
// back in the overview, while one of its menus is open, and when the pointer
// pushes against the top edge. The bar gives up its strut, so maximized
// windows use the whole screen.
//
// The bar slides out with translation_y and stays visible as far as Clutter
// is concerned: GNOME already shows and hides it for fullscreen windows, and
// the two would fight. Its input region moves off screen with it.

import Clutter from 'gi://Clutter';
import GLib from 'gi://GLib';
import Meta from 'gi://Meta';
import Shell from 'gi://Shell';

import * as Layout from 'resource:///org/gnome/shell/ui/layout.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';

const SLIDE_MS = 200;
// Same as Hide Top Bar's defaults.
const PRESSURE_THRESHOLD = 100;
const PRESSURE_TIMEOUT = 1000;
// The edge barrier stops this far short of a hot corner, so it never holds a
// pointer heading for the corner after the corner has let it go.
const HOT_CORNER_GAP = 100;
// While revealed by the pointer, how often to check whether it has left.
const POINTER_POLL_MS = 250;

const OVERLAPPING_TYPES = [
    Meta.WindowType.NORMAL,
    Meta.WindowType.DIALOG,
    Meta.WindowType.MODAL_DIALOG,
    Meta.WindowType.UTILITY,
];

export class TopBar {
    constructor(settings, injections) {
        this._settings = settings;
        this._injections = injections;
        this._enabled = false;
        this._signals = [];
        this._focusSignals = [];
        this._laterId = 0;
        this._pollId = 0;
        this._hidden = false;
        this._revealed = false; // shown by the pointer over a window
        this._unredirectDisabled = false;
        this._barrier = null;
        this._pressure = null;

        this._menuSignals = new Map(); // panel menu -> handler ids

        const self = this;
        // A panel menu opened from the keyboard needs the bar on screen. The
        // menus already there are watched in _enable(); these come later.
        injections.overrideMethod(PopupMenu.PopupMenuManager.prototype, 'addMenu',
            original => function (menu, ...args) {
                const result = original.call(this, menu, ...args);
                if (this === Main.panel.menuManager && self._enabled)
                    self._watchMenu(menu);
                return result;
            });

        this._settingsId = settings.connect('changed', (_s, key) => {
            if (key === 'topbar-autohide')
                this._sync();
            else if (key === 'topbar-reveal-at-edge')
                this._updateBarrier();
        });
        this._sync();
    }

    _sync() {
        if (this._settings.get_boolean('topbar-autohide'))
            this._enable();
        else
            this._disable();
    }

    _enable() {
        if (this._enabled)
            return;
        this._enabled = true;
        const panelBox = Main.layoutManager.panelBox;

        this._setStruts(false);

        const on = (obj, signal, fn) => this._signals.push([obj, obj.connect(signal, fn)]);
        const check = () => this._queueCheck();
        on(global.display, 'restacked', check);
        on(global.display, 'notify::focus-window', () => this._watchFocus());
        on(global.display, 'in-fullscreen-changed', check);
        on(global.display, 'workareas-changed', check);
        on(global.window_manager, 'size-changed', check);
        on(global.window_manager, 'minimize', check);
        on(global.window_manager, 'unminimize', check);
        on(global.window_manager, 'map', check);
        on(global.window_manager, 'destroy', check);
        on(global.workspace_manager, 'active-workspace-changed', check);
        on(Main.overview, 'showing', check);
        on(Main.overview, 'hidden', check);
        on(panelBox, 'notify::height', () => {
            this._updateBarrier();
            this._updateOffsets();
            check();
        });
        on(Main.layoutManager, 'monitors-changed', () => {
            this._updateBarrier();
            check();
        });
        on(Main.layoutManager, 'hot-corners-changed', () => this._updateBarrier());

        Main.panel.menuManager._menus.forEach(menu => this._watchMenu(menu));
        this._watchFocus();
        this._updateBarrier();
        this._updateOffsets();
        this._check();
    }

    _disable() {
        if (!this._enabled)
            return;
        this._enabled = false;
        for (const [obj, id] of this._signals)
            obj.disconnect(id);
        this._signals = [];
        this._unwatchFocus();
        for (const [menu, ids] of this._menuSignals)
            ids.forEach(id => menu.disconnect(id));
        this._menuSignals.clear();
        this._removeBarrier();
        this._stopPolling();
        if (this._laterId)
            global.compositor.get_laters().remove(this._laterId);
        this._laterId = 0;

        const panelBox = Main.layoutManager.panelBox;
        panelBox.remove_transition('translation-y');
        panelBox.translation_y = 0;
        this._hidden = false;
        this._revealed = false;
        this._setUnredirectDisabled(false);
        this._clearOffsets();
        // Giving the strut back while locked would shrink the work area and
        // push windows down; the shell tracks it again on unlock.
        this._setStruts(Main.sessionMode.currentMode !== 'unlock-dialog');
    }

    _setStruts(affectsStruts) {
        const panelBox = Main.layoutManager.panelBox;
        Main.layoutManager.untrackChrome(panelBox);
        Main.layoutManager.trackChrome(panelBox, {affectsStruts, trackFullscreen: true});
    }

    // Moving the focused window can put it under the bar without any of the
    // display-wide signals firing.
    _watchFocus() {
        this._unwatchFocus();
        const window = global.display.focus_window;
        if (window) {
            for (const signal of ['position-changed', 'size-changed'])
                this._focusSignals.push([window, window.connect(signal, () => this._queueCheck())]);
        }
        this._queueCheck();
    }

    _watchMenu(menu) {
        if (this._menuSignals.has(menu))
            return;
        this._menuSignals.set(menu, [
            menu.connect('open-state-changed', () => this._check()),
            // Panel menus live as long as the shell, but extensions' don't.
            menu.connect('destroy', () => this._menuSignals.delete(menu)),
        ]);
    }

    _unwatchFocus() {
        for (const [obj, id] of this._focusSignals)
            obj.disconnect(id);
        this._focusSignals = [];
    }

    _queueCheck() {
        if (this._laterId || !this._enabled)
            return;
        this._laterId = global.compositor.get_laters().add(Meta.LaterType.BEFORE_REDRAW, () => {
            this._laterId = 0;
            this._check();
            return false;
        });
    }

    _check() {
        if (!this._enabled)
            return;
        const keepShown = Main.overview.visible || Main.overview.animationInProgress ||
            !!Main.panel.menuManager.activeMenu;
        if (keepShown) {
            this._revealed = false;
            this._show();
        } else if (this._overlapped()) {
            if (!this._revealed)
                this._hide();
        } else {
            this._revealed = false;
            this._show();
        }
    }

    // Whether a window of the focused app (or the focused window, if it has
    // no app) covers part of the bar.
    _overlapped() {
        const monitor = Main.layoutManager.primaryMonitor;
        if (!monitor)
            return false;
        const height = Main.layoutManager.panelBox.height;
        const tracker = Shell.WindowTracker.get_default();
        const focusApp = tracker.focus_app;
        const focusWindow = global.display.focus_window;
        if (!focusApp && !focusWindow)
            return false;

        const workspace = global.workspace_manager.get_active_workspace();
        for (const w of workspace.list_windows()) {
            if (w.minimized || !w.showing_on_its_workspace() || w.get_monitor() !== monitor.index)
                continue;
            if (!OVERLAPPING_TYPES.includes(w.get_window_type()))
                continue;
            if (focusApp ? tracker.get_window_app(w) !== focusApp : w !== focusWindow)
                continue;
            const r = w.get_frame_rect();
            if (r.x < monitor.x + monitor.width && r.x + r.width > monitor.x &&
                r.y < monitor.y + height && r.y + r.height > monitor.y)
                return true;
        }
        return false;
    }

    _slide(hidden) {
        const panelBox = Main.layoutManager.panelBox;
        this._hidden = hidden;
        panelBox.ease({
            translation_y: hidden ? -panelBox.height : 0,
            duration: SLIDE_MS,
            mode: Clutter.AnimationMode.EASE_OUT_QUAD,
        });
        this._updateOffsets();
    }

    _show() {
        // A bar shown over a fullscreen or maximized window must stop it
        // from being unredirected, or the bar wouldn't be painted.
        this._setUnredirectDisabled(this._revealed);
        if (this._hidden)
            this._slide(false);
    }

    _hide() {
        this._stopPolling();
        this._setUnredirectDisabled(false);
        if (!this._hidden)
            this._slide(true);
    }

    _setUnredirectDisabled(disabled) {
        if (disabled === this._unredirectDisabled)
            return;
        if (disabled)
            global.compositor.disable_unredirect();
        else
            global.compositor.enable_unredirect();
        this._unredirectDisabled = disabled;
    }

    // Pointer reveal: a pressure barrier along the primary monitor's top edge,
    // short of its hot corner.
    _updateBarrier() {
        this._removeBarrier();
        const monitor = Main.layoutManager.primaryMonitor;
        if (!this._enabled || !monitor || !this._settings.get_boolean('topbar-reveal-at-edge'))
            return;
        this._pressure = new Layout.PressureBarrier(PRESSURE_THRESHOLD, PRESSURE_TIMEOUT,
            Shell.ActionMode.NORMAL);
        this._pressure.connect('trigger', () => this._reveal());
        let x1 = monitor.x, x2 = monitor.x + monitor.width;
        if (Main.layoutManager.hotCorners[monitor.index]) {
            if (Clutter.get_default_text_direction() === Clutter.TextDirection.RTL)
                x2 -= HOT_CORNER_GAP;
            else
                x1 += HOT_CORNER_GAP;
        }
        this._barrier = new Meta.Barrier({
            backend: global.backend,
            x1, x2,
            y1: monitor.y, y2: monitor.y,
            directions: Meta.BarrierDirection.POSITIVE_Y,
        });
        this._pressure.addBarrier(this._barrier);
    }

    _removeBarrier() {
        if (this._barrier) {
            this._pressure.removeBarrier(this._barrier);
            this._barrier.destroy();
        }
        this._pressure?.destroy();
        this._barrier = null;
        this._pressure = null;
    }

    _reveal() {
        if (!this._hidden)
            return;
        this._revealed = true;
        this._show();
        this._startPolling();
    }

    // Hide again once the pointer has left the bar and no menu is open.
    _startPolling() {
        this._stopPolling();
        this._pollId = GLib.timeout_add(GLib.PRIORITY_DEFAULT, POINTER_POLL_MS, () => {
            const panelBox = Main.layoutManager.panelBox;
            const [, y] = global.get_pointer();
            const monitor = Main.layoutManager.primaryMonitor;
            const over = monitor && y < monitor.y + panelBox.height;
            if (over || Main.panel.menuManager.activeMenu)
                return GLib.SOURCE_CONTINUE;
            this._pollId = 0;
            this._revealed = false;
            this._check();
            return GLib.SOURCE_REMOVE;
        });
    }

    _stopPolling() {
        if (this._pollId)
            GLib.source_remove(this._pollId);
        this._pollId = 0;
    }

    // Without the strut, the overview and notification banners start at the
    // very top. Move them below the bar while it's on screen.
    _updateOffsets() {
        if (!this._enabled)
            return;
        const height = Main.layoutManager.panelBox.height;
        const scale = Main.layoutManager.primaryMonitor?.geometry_scale ?? 1;
        Main.overview._overview?._controls?._searchEntryBin?.set_style(
            `padding-top: ${height / scale}px;`);
        Main.messageTray.translation_y = this._hidden ? 0 : height;
    }

    _clearOffsets() {
        Main.overview._overview?._controls?._searchEntryBin?.set_style(null);
        Main.messageTray.translation_y = 0;
    }

    destroy() {
        this._settings.disconnect(this._settingsId);
        this._disable();
    }
}
