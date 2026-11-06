// Preferences page for following the system power mode: what each mode allows
// of the three effects that cost something.

import Adw from 'gi://Adw';
import Gio from 'gi://Gio';
import Gtk from 'gi://Gtk';

import {gettext as _} from 'resource:///org/gnome/Shell/Extensions/js/extensions/prefs.js';

const MODES = () => [
    ['performance', _('Performance'), 'power-profile-performance-symbolic'],
    ['balanced', _('Balanced'), 'power-profile-balanced-symbolic'],
    ['power-saver', _('Power Saver'), 'power-profile-power-saver-symbolic'],
];

// [feature, title, [[level, label], ...]]
const FEATURES = () => [
    ['popups', _('Menu and Popup Blur'), [
        ['full', _('Full')],
        ['light', _('Wallpaper Only')],
        ['off', _('Off')],
    ]],
    ['transitions', _('Transitions'), [
        ['full', _('Full')],
        ['light', _('Light Effects Only')],
        ['off', _('None')],
    ]],
    ['animated', _('Animated Wallpapers'), [
        ['full', _('Play')],
        ['light', _('Still Frame')],
    ]],
];

function levelRow(settings, key, title, levels) {
    const row = new Adw.ComboRow({title, model: Gtk.StringList.new(levels.map(([, label]) => label))});
    const sync = () => {
        // 'off' for animated wallpapers is a still frame too.
        const index = levels.findIndex(([level]) => level === settings.get_string(key));
        row.selected = index >= 0 ? index : levels.length - 1;
    };
    sync();
    const id = settings.connect(`changed::${key}`, sync);
    row.connect('destroy', () => settings.disconnect(id));
    row.connect('notify::selected', () => {
        const level = levels[row.selected]?.[0];
        if (level && level !== settings.get_string(key))
            settings.set_string(key, level);
    });
    return row;
}

export function powerPage(window, settings) {
    const page = new Adw.PreferencesPage({title: _('Power'), icon_name: 'power-profile-balanced-symbolic'});

    const group = new Adw.PreferencesGroup({
        title: _('Power Modes'),
        description: _('Only menu and popup blur, transitions and animated wallpapers cost noticeably more when used; the rest of Wallslæd costs about nothing. In each power mode they can be turned down. Nothing switched off on the other pages is ever switched back on here.'),
    });
    page.add(group);

    const adapt = new Adw.SwitchRow({
        title: _('Follow the Power Mode'),
        subtitle: _('Change the power mode in the system menu, at the top right'),
    });
    settings.bind('power-adapt', adapt, 'active', Gio.SettingsBindFlags.DEFAULT);
    group.add(adapt);

    // The current mode, as the system reports it.
    const current = new Adw.ActionRow({title: _('Current Mode')});
    group.add(current);
    const names = Object.fromEntries(MODES().map(([mode, name]) => [mode, name]));
    let proxy = null;
    try {
        proxy = Gio.DBusProxy.new_for_bus_sync(Gio.BusType.SYSTEM, Gio.DBusProxyFlags.DO_NOT_AUTO_START, null,
            'org.freedesktop.UPower.PowerProfiles', '/org/freedesktop/UPower/PowerProfiles',
            'org.freedesktop.UPower.PowerProfiles', null);
    } catch (e) {
        console.warn(`Wallslæd: power modes: ${e.message}`);
    }
    const showMode = () => {
        const mode = proxy?.get_cached_property('ActiveProfile')?.unpack();
        current.subtitle = names[mode] ?? _('Unknown: no power mode service, so every effect stays as set');
    };
    showMode();
    if (proxy) {
        const proxyId = proxy.connect('g-properties-changed', showMode);
        page.connect('destroy', () => proxy.disconnect(proxyId));
    }

    for (const [mode, name, icon] of MODES()) {
        const expander = new Adw.ExpanderRow({title: name});
        expander.add_prefix(new Gtk.Image({icon_name: icon}));
        settings.bind('power-adapt', expander, 'sensitive', Gio.SettingsBindFlags.GET);
        for (const [feature, title, levels] of FEATURES())
            expander.add_row(levelRow(settings, `${mode}-${feature}`, title, levels));
        group.add(expander);
    }

    const notes = new Adw.PreferencesGroup({
        title: _('What the Levels Do'),
        description: _('Wallpaper Only: menus show the wallpaper frosted instead of blurring the windows behind them, which costs nothing per frame. Light Effects Only: Random picks from Crossfade, Zoom, Pixelate, Ripple and Light Leak, and any other effect plays as a crossfade. Still Frame: animated wallpapers pause; music videos keep playing.'),
    });
    page.add(notes);
    return page;
}
