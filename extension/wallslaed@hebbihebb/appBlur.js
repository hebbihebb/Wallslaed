// The blur GNOME 51 draws behind app windows that ask for it (the Wayland
// ext-background-effect protocol; GTK 4.24's backdrop-filter uses it). GNOME
// only offers a compositor-wide policy: radius, saturation and noise. This
// sets the radius while the setting is on, and puts GNOME's values back when
// it's off or the extension goes.

export class AppBlur {
    constructor(settings) {
        this._settings = settings;
        const [radius, saturation, noise] = global.compositor.get_background_blur_params();
        this._original = {radius, saturation, noise};
        this._settingsId = settings.connect('changed', (_s, key) => {
            if (key === 'blur-apps' || key === 'blur-apps-strength')
                this._sync();
        });
        this._sync();
    }

    _sync() {
        const {radius, saturation, noise} = this._original;
        const ours = this._settings.get_boolean('blur-apps')
            ? this._settings.get_uint('blur-apps-strength') : radius;
        global.compositor.set_background_blur_params(ours, saturation, noise);
    }

    destroy() {
        this._settings.disconnect(this._settingsId);
        const {radius, saturation, noise} = this._original;
        global.compositor.set_background_blur_params(radius, saturation, noise);
    }
}
