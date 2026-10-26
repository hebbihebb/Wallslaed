// The lock screen's blur. GNOME already blurs the wallpaper there (radius 90,
// brightness 0.65); this only makes it follow the blur settings, so it looks
// like the rest of the shell. GNOME builds the unlock dialog just before the
// session switches to the lock screen, so this stays in place in both modes.

import St from 'gi://St';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as UnlockDialog from 'resource:///org/gnome/shell/ui/unlockDialog.js';

export class LockBlur {
    constructor(settings, injections) {
        const self = this;
        this._settings = settings;
        this._dialogs = new Map(); // unlock dialog -> destroy handler id

        injections.overrideMethod(UnlockDialog.UnlockDialog.prototype, '_updateBackgroundEffects',
            original => function (...args) {
                const result = original.apply(this, args);
                self._track(this);
                self._apply(this);
                return result;
            });

        this._settingsId = settings.connect('changed', (_s, key) => {
            if (key === 'blur-lockscreen' || key === 'blur-strength' || key === 'blur-brightness')
                this._refresh();
        });
        // The shell may have built its unlock dialog before this was enabled.
        this._refresh();
    }

    _track(dialog) {
        if (!this._dialogs.has(dialog))
            this._dialogs.set(dialog, dialog.connect('destroy', () => this._dialogs.delete(dialog)));
    }

    _refresh() {
        const current = Main.screenShield?._dialog;
        if (current instanceof UnlockDialog.UnlockDialog)
            this._track(current);
        this._dialogs.forEach((_id, dialog) => dialog._updateBackgroundEffects());
    }

    _apply(dialog) {
        if (!this._settings.get_boolean('blur-lockscreen'))
            return;
        const scale = St.ThemeContext.get_for_stage(global.stage).scale_factor;
        for (const widget of dialog._backgroundGroup) {
            widget.get_effect('blur')?.set({
                radius: this._settings.get_uint('blur-strength') * 2 * scale,
                brightness: this._settings.get_double('blur-brightness'),
            });
        }
    }

    // Call after the injections are cleared: dialogs still up go back to
    // GNOME's own blur.
    destroy() {
        this._settings.disconnect(this._settingsId);
        for (const [dialog, id] of this._dialogs) {
            dialog.disconnect(id);
            dialog._updateBackgroundEffects();
        }
        this._dialogs.clear();
    }
}
