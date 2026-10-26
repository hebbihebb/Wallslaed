// Preferences page for the shell's look: blur behind its surfaces, and the
// top bar hiding over windows.

import Adw from 'gi://Adw';
import Gio from 'gi://Gio';
import Gtk from 'gi://Gtk';

import {gettext as _} from 'resource:///org/gnome/Shell/Extensions/js/extensions/prefs.js';

function switches(group, settings, rows) {
    for (const [key, title, subtitle] of rows) {
        const row = new Adw.SwitchRow({title, subtitle: subtitle ?? ''});
        settings.bind(key, row, 'active', Gio.SettingsBindFlags.DEFAULT);
        group.add(row);
    }
}

export function appearancePage(window, settings) {
    const page = new Adw.PreferencesPage({title: _('Appearance'), icon_name: 'applications-graphics-symbolic'});

    const blur = new Adw.PreferencesGroup({
        title: _('Blur'),
        description: _('The overview and top bar show a small blurred copy of the wallpaper, made once per wallpaper. Menus and popups blur what is behind them only while they are open.'),
    });
    page.add(blur);

    const strength = new Adw.SpinRow({
        title: _('Strength'),
        subtitle: _('Pixels'),
        adjustment: new Gtk.Adjustment({lower: 0, upper: 100, step_increment: 5, page_increment: 10}),
    });
    settings.bind('blur-strength', strength, 'value', Gio.SettingsBindFlags.DEFAULT);
    blur.add(strength);

    const brightness = new Adw.SpinRow({
        title: _('Brightness'),
        subtitle: _('Lower darkens the blurred wallpaper so text stays readable'),
        digits: 2,
        adjustment: new Gtk.Adjustment({lower: 0.2, upper: 1, step_increment: 0.05, page_increment: 0.1}),
    });
    settings.bind('blur-brightness', brightness, 'value', Gio.SettingsBindFlags.DEFAULT);
    blur.add(brightness);

    switches(blur, settings, [
        ['blur-overview', _('Overview')],
        ['blur-panel', _('Top Bar')],
        ['blur-dash', _('See-Through Dash'), _('Needs the overview blur')],
        ['blur-popups', _('Menus and Popups'), _('Panel menus, quick settings, notifications, dialogs, the window switcher')],
        ['blur-app-folders', _('App Folders')],
        ['blur-screenshot', _('Screenshot Tool')],
    ]);

    const topBar = new Adw.PreferencesGroup({title: _('Top Bar')});
    page.add(topBar);
    switches(topBar, settings, [
        ['topbar-autohide', _('Hide Over Windows'),
            _('Hide the top bar while a window of the focused app touches it, so maximized windows use the whole screen')],
        ['topbar-reveal-at-edge', _('Show at the Top Edge'),
            _('Push the pointer against the top of the screen to bring the hidden bar back')],
    ]);
    return page;
}
