// Preferences page for music videos: download from a link, manage the saved
// ones, and pick one to play on the desktop. Downloads run in the daemon, so
// closing this window doesn't stop them.

import Adw from 'gi://Adw';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Gtk from 'gi://Gtk';

import {gettext as _} from 'resource:///org/gnome/Shell/Extensions/js/extensions/prefs.js';

import {BUS_NAME, IFACE_XML, OBJECT_PATH, unpack} from './iface.js';

const Proxy = Gio.DBusProxy.makeProxyWrapper(IFACE_XML);
Gio._promisify(Gio.File.prototype, 'trash_async');

const HEIGHTS = [480, 720, 1080, 1440, 2160];

function musicFolder(settings) {
    const f = settings.get_string('music-folder');
    if (f.startsWith('~/'))
        return GLib.build_filenamev([GLib.get_home_dir(), f.slice(2)]);
    return f || GLib.build_filenamev([
        GLib.get_user_special_dir(GLib.UserDirectory.DIRECTORY_VIDEOS) ?? GLib.build_filenamev([GLib.get_home_dir(), 'Videos']),
        'Music',
    ]);
}

function iconButton(icon, tooltip, onClick) {
    const b = new Gtk.Button({icon_name: icon, tooltip_text: tooltip, valign: Gtk.Align.CENTER});
    b.add_css_class('flat');
    b.connect('clicked', onClick);
    return b;
}

export function musicPage(window, settings) {
    const page = new Adw.PreferencesPage({title: _('Music Videos'), icon_name: 'audio-x-generic-symbolic'});
    const toast = title => window.add_toast(new Adw.Toast({title, timeout: 4}));
    let state = null;
    let refreshLibrary = () => {};

    const proxy = new Proxy(Gio.DBus.session, BUS_NAME, OBJECT_PATH, (p, error) => {
        if (error)
            return;
        p.GetStateAsync().then(([s]) => {
            state = unpack(s);
            refreshLibrary();
        }).catch(() => {});
        p.ListDownloadsAsync().then(([list]) => {
            for (const [id, title, fraction, status] of list)
                onProgress(id, title, fraction, status);
        }).catch(() => {});
    }, null, Gio.DBusProxyFlags.DO_NOT_AUTO_START);
    const call = (method, ...args) => proxy[`${method}Async`](...args)
        .catch(e => toast(e.message.replace(/^GDBus\.Error:[^ ]+ /, '')));

    // Download
    const downloadGroup = new Adw.PreferencesGroup({
        title: _('Download'),
        description: _('Paste a YouTube (or other video site) link. Downloads use yt-dlp; only download videos you have the right to keep.'),
    });
    page.add(downloadGroup);

    const urlRow = new Adw.EntryRow({title: _('Video Link'), show_apply_button: true});
    urlRow.connect('apply', () => {
        const url = urlRow.text.trim();
        if (!url)
            return;
        call('Download', url).then(reply => {
            if (reply)
                urlRow.text = '';
        });
    });
    downloadGroup.add(urlRow);

    const rows = new Map(); // download id -> {row, bar, cancel}
    const onProgress = (id, title, fraction, status) => {
        let r = rows.get(id);
        if (!r) {
            const row = new Adw.ActionRow({title: GLib.markup_escape_text(title, -1)});
            const bar = new Gtk.ProgressBar({valign: Gtk.Align.CENTER, width_request: 120});
            const cancel = iconButton('process-stop-symbolic', _('Cancel'), () => call('CancelDownload', id));
            row.add_suffix(bar);
            row.add_suffix(cancel);
            downloadGroup.add(row);
            r = {row, bar, cancel};
            rows.set(id, r);
        }
        if (title)
            r.row.title = GLib.markup_escape_text(title, -1);
        r.bar.fraction = fraction;
        const finished = ['done', 'cancelled'].includes(status) || status.startsWith('error:');
        r.cancel.visible = !finished;
        r.bar.visible = !finished;
        r.row.subtitle = GLib.markup_escape_text({
            queued: _('Waiting'),
            downloading: _('Downloading… %d%%').format(Math.round(fraction * 100)),
            processing: _('Finishing…'),
            done: _('Downloaded'),
            cancelled: _('Cancelled'),
        }[status] ?? status.replace(/^error:/, _('Failed: ')), -1);
        if (status === 'cancelled') {
            downloadGroup.remove(r.row);
            rows.delete(id);
        }
        if (status === 'done')
            refreshLibrary();
    };
    const progressId = proxy.connectSignal('DownloadProgress',
        (_p, _sender, [id, title, fraction, status]) => onProgress(id, title, fraction, status));
    const changedId = proxy.connectSignal('Changed', (_p, _sender, [s]) => {
        const media = state?.media;
        state = unpack(s);
        if (state.media !== media)
            refreshLibrary();
    });
    window.connect('close-request', () => {
        proxy.disconnectSignal(progressId);
        proxy.disconnectSignal(changedId);
        return false;
    });

    // Saved videos
    const library = new Adw.PreferencesGroup({title: _('Saved')});
    library.set_header_suffix(iconButton('view-refresh-symbolic', _('Refresh'), () => refreshLibrary()));
    page.add(library);
    let libraryRows = [];
    const empty = new Adw.ActionRow({title: _('No music videos yet'), sensitive: false});

    refreshLibrary = () => {
        proxy.ListMediaAsync().then(([list]) => {
            for (const row of libraryRows)
                library.remove(row);
            libraryRows = [];
            for (const item of list.map(unpack)) {
                const row = new Adw.ActionRow({title: GLib.markup_escape_text(item.title, -1)});
                if (item.thumbnail) {
                    const pic = Gtk.Picture.new_for_filename(item.thumbnail);
                    pic.content_fit = Gtk.ContentFit.COVER;
                    pic.set_size_request(64, 36);
                    pic.valign = Gtk.Align.CENTER;
                    row.add_prefix(pic);
                }
                const playing = item.path === state?.media;
                if (playing) {
                    row.subtitle = _('Playing on the desktop');
                    row.add_suffix(iconButton('media-playback-stop-symbolic', _('Stop'), () => call('StopMedia')));
                } else {
                    row.add_suffix(iconButton('media-playback-start-symbolic', _('Play on Desktop'),
                        () => call('PlayMedia', item.path)));
                }
                row.add_suffix(iconButton('user-trash-symbolic', _('Move to Trash'), async () => {
                    if (playing)
                        await call('StopMedia');
                    try {
                        await Gio.File.new_for_path(item.path).trash_async(GLib.PRIORITY_DEFAULT, null);
                        if (item.thumbnail)
                            await Gio.File.new_for_path(item.thumbnail).trash_async(GLib.PRIORITY_DEFAULT, null);
                    } catch (e) {
                        toast(e.message);
                    }
                    refreshLibrary();
                }));
                library.add(row);
                libraryRows.push(row);
            }
            if (!libraryRows.length) {
                library.add(empty);
                libraryRows.push(empty);
            }
        }).catch(() => {});
    };

    // Settings
    const options = new Adw.PreferencesGroup({
        title: _('Options'),
        description: _('Music videos play with sound and hold the wallpaper rotation until you stop them or pick another wallpaper.'),
    });
    page.add(options);

    const folderRow = new Adw.ActionRow({title: _('Folder')});
    const showFolder = () => {
        folderRow.subtitle = GLib.markup_escape_text(musicFolder(settings), -1);
    };
    showFolder();
    settings.connect('changed::music-folder', () => {
        showFolder();
        refreshLibrary();
    });
    const choose = iconButton('folder-open-symbolic', _('Choose Folder'), () => {
        const dialog = new Gtk.FileDialog({title: _('Music Video Folder')});
        dialog.select_folder(window, null, (d, res) => {
            try {
                const file = d.select_folder_finish(res);
                if (file?.get_path())
                    settings.set_string('music-folder', file.get_path());
            } catch {
                // Cancelled.
            }
        });
    });
    folderRow.add_suffix(choose);
    folderRow.activatable_widget = choose;
    options.add(folderRow);

    const volumeRow = new Adw.ActionRow({title: _('Volume')});
    const volume = new Gtk.Scale({
        adjustment: new Gtk.Adjustment({lower: 0, upper: 1, step_increment: 0.05}),
        hexpand: true,
        valign: Gtk.Align.CENTER,
        width_request: 200,
    });
    settings.bind('music-volume', volume.adjustment, 'value', Gio.SettingsBindFlags.DEFAULT);
    volumeRow.add_suffix(volume);
    options.add(volumeRow);

    const quality = new Adw.ComboRow({
        title: _('Download Quality'),
        subtitle: _('Highest resolution to download'),
        model: Gtk.StringList.new(HEIGHTS.map(h => `${h}p`)),
    });
    const syncQuality = () => {
        const h = settings.get_uint('music-max-height');
        const i = HEIGHTS.findIndex(x => x >= h);
        quality.selected = i < 0 ? HEIGHTS.length - 1 : i;
    };
    syncQuality();
    quality.connect('notify::selected', () => {
        const h = HEIGHTS[quality.selected];
        if (h !== settings.get_uint('music-max-height'))
            settings.set_uint('music-max-height', h);
    });
    settings.connect('changed::music-max-height', syncQuality);
    options.add(quality);

    return page;
}
