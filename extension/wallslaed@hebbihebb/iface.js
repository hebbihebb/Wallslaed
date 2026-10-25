// The daemon's D-Bus interface, shared by the extension and its preferences.
// No Shell imports here: preferences run in a separate process.

import GLib from 'gi://GLib';

export const BUS_NAME = 'io.github.hebbihebb.Wallslaed';
export const OBJECT_PATH = '/io/github/hebbihebb/Wallslaed';

export const IFACE_XML = `
<node>
  <interface name="io.github.hebbihebb.Wallslaed">
    <method name="Next"/>
    <method name="Previous"/>
    <method name="SetPaused"><arg type="b" name="paused" direction="in"/></method>
    <method name="TogglePause"/>
    <method name="ToggleFavorite"/>
    <method name="Ban"/>
    <method name="Rescan"/>
    <method name="ShowInFiles"/>
    <method name="GetState"><arg type="a{sv}" name="state" direction="out"/></method>
    <method name="PlayMedia"><arg type="s" name="path" direction="in"/></method>
    <method name="StopMedia"/>
    <method name="ReportPosition">
      <arg type="s" name="path" direction="in"/>
      <arg type="d" name="seconds" direction="in"/>
    </method>
    <method name="ListMedia"><arg type="aa{sv}" name="items" direction="out"/></method>
    <method name="Download">
      <arg type="s" name="url" direction="in"/>
      <arg type="s" name="id" direction="out"/>
    </method>
    <method name="CancelDownload"><arg type="s" name="id" direction="in"/></method>
    <method name="ListDownloads"><arg type="a(ssds)" name="downloads" direction="out"/></method>
    <signal name="Changed"><arg type="a{sv}" name="state"/></signal>
    <signal name="DownloadProgress">
      <arg type="s" name="id"/>
      <arg type="s" name="title"/>
      <arg type="d" name="fraction"/>
      <arg type="s" name="status"/>
    </signal>
  </interface>
</node>`;

export function unpack(dict) {
    const out = {};
    for (const [k, v] of Object.entries(dict))
        out[k] = v instanceof GLib.Variant ? v.recursiveUnpack() : v;
    return out;
}
