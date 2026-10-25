// Preferences page for finding animated wallpapers on Pexels and Pixabay.
// Both have free APIs whose licence allows using the videos as wallpapers.
// Downloads land in "<wallpaper folder>/Animated", where the daemon's folder
// watcher picks them up; they're written under a .part name first, so a
// half-downloaded file never joins the rotation.

import Adw from 'gi://Adw';
import Gdk from 'gi://Gdk';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Gtk from 'gi://Gtk';
import Soup from 'gi://Soup?version=3.0';

import {gettext as _} from 'resource:///org/gnome/Shell/Extensions/js/extensions/prefs.js';

Gio._promisify(Soup.Session.prototype, 'send_and_read_async');
Gio._promisify(Soup.Session.prototype, 'send_async');
Gio._promisify(Gio.File.prototype, 'replace_async');
Gio._promisify(Gio.OutputStream.prototype, 'splice_async');

const PER_PAGE = 24;

// Each source turns a search into a request, and its reply into items of
// {id, author, page, duration, thumb, files: [{url, width, height}]}.
const SOURCES = [
    {
        name: 'Pexels',
        key: 'pexels-api-key',
        signup: 'https://www.pexels.com/api/',
        request(query, key) {
            const msg = Soup.Message.new('GET', 'https://api.pexels.com/videos/search?' +
                `query=${encodeURIComponent(query)}&per_page=${PER_PAGE}&orientation=landscape`);
            msg?.request_headers.append('Authorization', key);
            return msg;
        },
        parse: json => (json.videos ?? []).map(v => ({
            id: `pexels-${v.id}`,
            author: v.user?.name ?? '',
            page: v.url,
            duration: v.duration,
            thumb: v.image,
            files: (v.video_files ?? [])
                .filter(f => f.link && f.file_type === 'video/mp4')
                .map(f => ({url: f.link, width: f.width, height: f.height})),
        })),
    },
    {
        name: 'Pixabay',
        key: 'pixabay-api-key',
        signup: 'https://pixabay.com/api/docs/',
        request: (query, key) => Soup.Message.new('GET', 'https://pixabay.com/api/videos/?' +
            `key=${encodeURIComponent(key)}&q=${encodeURIComponent(query)}&per_page=${PER_PAGE}&safesearch=true`),
        parse: json => (json.hits ?? []).map(h => ({
            id: `pixabay-${h.id}`,
            author: h.user ?? '',
            page: h.pageURL,
            duration: h.duration,
            thumb: h.videos?.medium?.thumbnail ?? h.videos?.small?.thumbnail,
            files: Object.values(h.videos ?? {})
                .filter(f => f?.url)
                .map(f => ({url: f.url, width: f.width, height: f.height})),
        })),
    },
];

function wallpaperFolder(settings) {
    const f = settings.get_string('folder');
    if (f.startsWith('~/'))
        return GLib.build_filenamev([GLib.get_home_dir(), f.slice(2)]);
    return f || GLib.get_user_special_dir(GLib.UserDirectory.DIRECTORY_PICTURES) || GLib.get_home_dir();
}

/** Tallest monitor, in physical pixels. */
function screenHeight() {
    const monitors = Gdk.Display.get_default()?.get_monitors();
    let best = 1080;
    for (let i = 0; monitors && i < monitors.get_n_items(); i++) {
        const m = monitors.get_item(i);
        best = Math.max(best, m.get_geometry().height * m.get_scale_factor());
    }
    return best;
}

/** The smallest file that still covers the screen, else the biggest there is. */
function pickFile(files, height) {
    const sorted = [...files].sort((a, b) => a.height - b.height);
    return sorted.find(f => f.height >= height) ?? sorted.at(-1);
}

function duration(secs) {
    const s = Math.round(secs ?? 0);
    return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`;
}

async function fetchBytes(session, msg, cancellable) {
    const bytes = await session.send_and_read_async(msg, GLib.PRIORITY_DEFAULT, cancellable);
    if (msg.get_status() !== Soup.Status.OK)
        throw new Error(`HTTP ${msg.get_status()} ${msg.get_reason_phrase() ?? ''}`.trim());
    return bytes;
}

/** Stream a URL to `dest`, via a .part file renamed once complete. */
async function fetchToFile(session, url, dest, cancellable) {
    const msg = Soup.Message.new('GET', url);
    if (!msg)
        throw new Error(_('Bad link'));
    const input = await session.send_async(msg, GLib.PRIORITY_DEFAULT, cancellable);
    if (msg.get_status() !== Soup.Status.OK)
        throw new Error(`HTTP ${msg.get_status()}`);
    const part = Gio.File.new_for_path(`${dest.get_path()}.part`);
    try {
        const out = await part.replace_async(null, false, Gio.FileCreateFlags.NONE,
            GLib.PRIORITY_DEFAULT, cancellable);
        await out.splice_async(input,
            Gio.OutputStreamSpliceFlags.CLOSE_SOURCE | Gio.OutputStreamSpliceFlags.CLOSE_TARGET,
            GLib.PRIORITY_DEFAULT, cancellable);
        part.move(dest, Gio.FileCopyFlags.OVERWRITE, null, null);
    } catch (e) {
        try {
            part.delete(null);
        } catch {
            // Never created.
        }
        throw e;
    }
}

export function browsePage(window, settings) {
    const page = new Adw.PreferencesPage({title: _('Browse'), icon_name: 'system-search-symbolic'});
    const toast = title => window.add_toast(new Adw.Toast({title, timeout: 4}));
    const session = new Soup.Session({user_agent: 'Wallslaed/0.1', timeout: 30});
    const cancellable = new Gio.Cancellable();
    const previews = GLib.build_filenamev([GLib.get_user_cache_dir(), 'wallslaed', 'previews']);

    window.connect('close-request', () => {
        cancellable.cancel();
        // Previews are only for browsing; don't keep them around.
        try {
            const dir = Gio.File.new_for_path(previews);
            for (const info of dir.enumerate_children('standard::name', 0, null))
                dir.get_child(info.get_name()).delete(null);
        } catch {
            // Nothing there.
        }
        return false;
    });

    // API keys
    const keys = new Adw.PreferencesGroup({
        title: _('Sources'),
        description: _('Both sites give out free API keys. Videos come with credit to their authors; please keep it when sharing.'),
    });
    page.add(keys);
    for (const source of SOURCES) {
        const row = new Adw.PasswordEntryRow({title: _('%s API Key').format(source.name)});
        settings.bind(source.key, row, 'text', Gio.SettingsBindFlags.DEFAULT);
        row.add_suffix(new Gtk.LinkButton({
            label: _('Get a Key'),
            uri: source.signup,
            valign: Gtk.Align.CENTER,
        }));
        keys.add(row);
    }

    // Search
    const search = new Adw.PreferencesGroup({title: _('Animated Wallpapers')});
    page.add(search);
    const sourceRow = new Adw.ComboRow({
        title: _('Site'),
        model: Gtk.StringList.new(SOURCES.map(s => s.name)),
    });
    search.add(sourceRow);
    const queryRow = new Adw.EntryRow({title: _('Search, e.g. “rain” or “neon city”'), show_apply_button: true});
    search.add(queryRow);

    const status = new Gtk.Label({wrap: true, margin_top: 12, css_classes: ['dim-label']});
    const results = new Gtk.FlowBox({
        selection_mode: Gtk.SelectionMode.NONE,
        homogeneous: true,
        min_children_per_line: 2,
        max_children_per_line: 3,
        column_spacing: 12,
        row_spacing: 12,
        margin_top: 12,
    });
    const resultsGroup = new Adw.PreferencesGroup();
    resultsGroup.add(status);
    resultsGroup.add(results);
    page.add(resultsGroup);

    const card = (item, source) => {
        const box = new Gtk.Box({orientation: Gtk.Orientation.VERTICAL, spacing: 6});
        box.add_css_class('card');

        const stack = new Gtk.Stack({height_request: 110, width_request: 180});
        const pic = new Gtk.Picture({content_fit: Gtk.ContentFit.COVER});
        stack.add_named(pic, 'thumb');
        box.append(stack);
        const thumb = item.thumb && Soup.Message.new('GET', item.thumb);
        if (thumb) {
            fetchBytes(session, thumb, cancellable)
                .then(bytes => (pic.paintable = Gdk.Texture.new_from_bytes(bytes)))
                .catch(() => {});
        }

        const credit = new Gtk.Label({
            label: `${duration(item.duration)} · ${item.author} · ${source.name}`,
            ellipsize: 3, // Pango.EllipsizeMode.END
            xalign: 0,
            margin_start: 8,
            margin_end: 8,
            css_classes: ['caption'],
        });
        box.append(credit);

        const buttons = new Gtk.Box({spacing: 6, margin_start: 8, margin_end: 8, margin_bottom: 8});
        const button = (icon, tooltip, onClick) => {
            const b = new Gtk.Button({icon_name: icon, tooltip_text: tooltip});
            b.add_css_class('flat');
            b.connect('clicked', onClick);
            buttons.append(b);
            return b;
        };

        let video = null;
        const preview = button('media-playback-start-symbolic', _('Preview'), async () => {
            if (video) {
                const playing = stack.visible_child_name === 'video';
                stack.visible_child_name = playing ? 'thumb' : 'video';
                video.media_stream?.set_playing(!playing);
                preview.icon_name = playing ? 'media-playback-start-symbolic' : 'media-playback-pause-symbolic';
                return;
            }
            const small = [...item.files].sort((a, b) => a.height - b.height).find(f => f.height >= 360) ?? item.files[0];
            if (!small)
                return;
            preview.sensitive = false;
            try {
                GLib.mkdir_with_parents(previews, 0o755);
                const file = Gio.File.new_for_path(GLib.build_filenamev([previews, `${item.id}.mp4`]));
                if (!file.query_exists(null))
                    await fetchToFile(session, small.url, file, cancellable);
                video = new Gtk.Video({file, autoplay: true, loop: true});
                video.media_stream?.set_muted(true);
                stack.add_named(video, 'video');
                stack.visible_child_name = 'video';
                preview.icon_name = 'media-playback-pause-symbolic';
            } catch (e) {
                if (!cancellable.is_cancelled())
                    toast(_('Preview failed: %s').format(e.message));
            }
            preview.sensitive = true;
        });

        const dir = GLib.build_filenamev([wallpaperFolder(settings), 'Animated']);
        const name = `${item.id} ${item.author}`.replace(/[/\\\0]/g, '_').slice(0, 120);
        const dest = Gio.File.new_for_path(GLib.build_filenamev([dir, `${name}.mp4`]));
        const download = button('folder-download-symbolic', _('Download as Wallpaper'), async () => {
            const file = pickFile(item.files, screenHeight());
            if (!file)
                return;
            download.sensitive = false;
            download.icon_name = 'content-loading-symbolic';
            try {
                GLib.mkdir_with_parents(dir, 0o755);
                await fetchToFile(session, file.url, dest, cancellable);
                download.icon_name = 'object-select-symbolic';
                download.tooltip_text = _('Saved to %s').format(dest.get_path());
                toast(_('Saved; it joins the rotation in a few seconds'));
            } catch (e) {
                download.icon_name = 'folder-download-symbolic';
                download.sensitive = true;
                if (!cancellable.is_cancelled())
                    toast(_('Download failed: %s').format(e.message));
            }
        });
        if (dest.query_exists(null)) {
            download.sensitive = false;
            download.icon_name = 'object-select-symbolic';
            download.tooltip_text = _('Already downloaded');
        }

        const spacer = new Gtk.Box({hexpand: true});
        buttons.append(spacer);
        button('web-browser-symbolic', _('Open on %s').format(source.name), () => {
            new Gtk.UriLauncher({uri: item.page}).launch(window, null, null);
        });
        box.append(buttons);
        return box;
    };

    let searching = null;
    const runSearch = async () => {
        const source = SOURCES[sourceRow.selected];
        const query = queryRow.text.trim();
        const key = settings.get_string(source.key).trim();
        if (!query)
            return;
        if (!key) {
            status.label = _('Add a %s API key above to search it.').format(source.name);
            return;
        }
        const msg = source.request(query, key);
        if (!msg)
            return;
        searching = msg;
        status.label = _('Searching…');
        results.remove_all();
        try {
            const bytes = await fetchBytes(session, msg, cancellable);
            if (searching !== msg)
                return;
            const items = source.parse(JSON.parse(new TextDecoder().decode(bytes.get_data())))
                .filter(i => i.files.length);
            status.label = items.length
                ? _('%d results from %s').format(items.length, source.name)
                : _('Nothing found.');
            for (const item of items)
                results.append(card(item, source));
        } catch (e) {
            if (searching === msg && !cancellable.is_cancelled())
                status.label = _('Search failed: %s').format(e.message);
        }
    };
    queryRow.connect('apply', runSearch);
    queryRow.connect('entry-activated', runSearch);
    sourceRow.connect('notify::selected', runSearch);

    return page;
}
