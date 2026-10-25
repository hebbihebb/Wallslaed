import Adw from 'gi://Adw';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Gtk from 'gi://Gtk';

import {ExtensionPreferences, gettext as _} from 'resource:///org/gnome/Shell/Extensions/js/extensions/prefs.js';

import {browsePage} from './prefsBrowse.js';
import {musicPage} from './prefsMusic.js';

// A function, because gettext only works once the preferences are loading.
const transitions = () => [
    ['none', _('None')],
    ['fade', _('Crossfade')],
    ['zoom', _('Zoom')],
    ['slide', _('Slide')],
    ['pixelate', _('Pixelate')],
    ['wipe', _('Wipe')],
    ['blur', _('Blur')],
    ['random', _('Random')],
];

function callDaemon(method) {
    Gio.DBus.session.call(
        'io.github.hebbihebb.Wallslaed', '/io/github/hebbihebb/Wallslaed',
        'io.github.hebbihebb.Wallslaed', method, null, null,
        Gio.DBusCallFlags.NO_AUTO_START, -1, null, null);
}

export default class WallslaedPreferences extends ExtensionPreferences {
    fillPreferencesWindow(window) {
        const settings = this.getSettings();
        window._settings = settings;
        window.set_default_size(640, 760);

        const page = new Adw.PreferencesPage({title: _('Wallpapers'), icon_name: 'preferences-desktop-wallpaper-symbolic'});
        window.add(page);
        window.add(musicPage(window, settings));
        window.add(browsePage(window, settings));

        // Wallpapers
        const library = new Adw.PreferencesGroup({title: _('Wallpapers')});
        page.add(library);

        const folderRow = new Adw.ActionRow({title: _('Folder')});
        const showFolder = () => {
            const f = settings.get_string('folder');
            folderRow.subtitle = f || GLib.get_user_special_dir(GLib.UserDirectory.DIRECTORY_PICTURES) || '';
        };
        showFolder();
        settings.connect('changed::folder', showFolder);
        const chooseButton = new Gtk.Button({
            icon_name: 'folder-open-symbolic',
            valign: Gtk.Align.CENTER,
            tooltip_text: _('Choose Folder'),
        });
        chooseButton.add_css_class('flat');
        chooseButton.connect('clicked', () => {
            const dialog = new Gtk.FileDialog({title: _('Wallpaper Folder')});
            dialog.select_folder(window, null, (d, res) => {
                try {
                    const file = d.select_folder_finish(res);
                    if (file)
                        settings.set_string('folder', file.get_path());
                } catch {
                    // Cancelled.
                }
            });
        });
        folderRow.add_suffix(chooseButton);
        folderRow.activatable_widget = chooseButton;
        library.add(folderRow);

        const recursive = new Adw.SwitchRow({title: _('Include Subfolders')});
        settings.bind('recursive', recursive, 'active', Gio.SettingsBindFlags.DEFAULT);
        library.add(recursive);

        // Minutes in the UI, seconds in GSettings. The lower limit matches the
        // schema's 5 seconds, and showing a value never writes one back, so a
        // setting made elsewhere is never rounded or clamped behind your back.
        const interval = new Adw.SpinRow({
            title: _('Change Every'),
            subtitle: _('Minutes'),
            digits: 2,
            adjustment: new Gtk.Adjustment({lower: 5 / 60, upper: 10080, step_increment: 1, page_increment: 15}),
        });
        let showingInterval = false;
        const showInterval = () => {
            showingInterval = true;
            interval.value = settings.get_uint('interval') / 60;
            showingInterval = false;
        };
        showInterval();
        interval.connect('notify::value', () => {
            if (showingInterval)
                return;
            const secs = Math.max(5, Math.round(interval.value * 60));
            if (secs !== settings.get_uint('interval'))
                settings.set_uint('interval', secs);
        });
        settings.connect('changed::interval', showInterval);
        library.add(interval);

        const paused = new Adw.SwitchRow({title: _('Pause Rotation')});
        settings.bind('paused', paused, 'active', Gio.SettingsBindFlags.DEFAULT);
        library.add(paused);

        const weight = new Adw.SpinRow({
            title: _('Favorite Weight'),
            subtitle: _('How many times per cycle a favorite is shown'),
            adjustment: new Gtk.Adjustment({lower: 1, upper: 10, step_increment: 1}),
        });
        settings.bind('favorite-weight', weight, 'value', Gio.SettingsBindFlags.DEFAULT);
        library.add(weight);

        const prescale = new Adw.SwitchRow({
            title: _('Shrink Large Images'),
            subtitle: _('Scale images to the screen size first. Saves a lot of memory with 4K and 8K wallpapers.'),
        });
        settings.bind('prescale', prescale, 'active', Gio.SettingsBindFlags.DEFAULT);
        library.add(prescale);

        const actions = new Adw.ActionRow({title: _('Library')});
        for (const [label, method] of [[_('Rescan'), 'Rescan'], [_('Next Wallpaper'), 'Next']]) {
            const b = new Gtk.Button({label, valign: Gtk.Align.CENTER});
            b.connect('clicked', () => callDaemon(method));
            actions.add_suffix(b);
        }
        library.add(actions);

        // Transitions
        const transitionGroup = new Adw.PreferencesGroup({
            title: _('Transitions'),
            description: _('Effects only run on the GPU for the length of the transition.'),
        });
        page.add(transitionGroup);

        const TRANSITIONS = transitions();
        const effect = new Adw.ComboRow({
            title: _('Effect'),
            model: Gtk.StringList.new(TRANSITIONS.map(([, label]) => label)),
        });
        const syncEffect = () => {
            // Dissolve was removed; the shell plays a crossfade for it instead.
            const current = settings.get_string('transition').replace(/^dissolve$/, 'fade');
            const i = TRANSITIONS.findIndex(([id]) => id === current);
            effect.selected = Math.max(0, i);
        };
        syncEffect();
        effect.connect('notify::selected', () => {
            const id = TRANSITIONS[effect.selected][0];
            if (id !== settings.get_string('transition'))
                settings.set_string('transition', id);
        });
        settings.connect('changed::transition', syncEffect);
        transitionGroup.add(effect);

        const duration = new Adw.SpinRow({
            title: _('Duration'),
            subtitle: _('Milliseconds'),
            adjustment: new Gtk.Adjustment({lower: 100, upper: 5000, step_increment: 100, page_increment: 500}),
        });
        settings.bind('transition-duration', duration, 'value', Gio.SettingsBindFlags.DEFAULT);
        transitionGroup.add(duration);

        // Animated wallpapers
        const animated = new Adw.PreferencesGroup({
            title: _('Animated Wallpapers'),
            description: _('GIFs are converted to video once, so they can be hardware-decoded.'),
        });
        page.add(animated);

        for (const [key, title, subtitle] of [
            ['animated-enabled', _('Include GIFs and Videos'), null],
            ['animated-pause-when-covered', _('Pause When Covered'),
                _('Stop animating while maximized or fullscreen windows cover every screen')],
            ['animated-pause-on-battery', _('Pause on Battery'),
                _('Show a still frame while running on battery')],
        ]) {
            const row = new Adw.SwitchRow({title, subtitle: subtitle ?? ''});
            settings.bind(key, row, 'active', Gio.SettingsBindFlags.DEFAULT);
            animated.add(row);
        }
    }
}
