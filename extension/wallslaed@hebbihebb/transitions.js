// Transition effects between wallpapers. Effects are attached to the outgoing
// background only for the length of the transition and removed afterwards, so
// they cost nothing while the desktop is idle.

import Clutter from 'gi://Clutter';
import Cogl from 'gi://Cogl';
import GObject from 'gi://GObject';
import Shell from 'gi://Shell';
import St from 'gi://St';

import * as Background from 'resource:///org/gnome/shell/ui/background.js';
import * as BoxPointer from 'resource:///org/gnome/shell/ui/boxpointer.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';

// The `random` pool. A stored 'dissolve' (removed) falls through to the
// crossfade. Newer effects join the pool once they're accepted on a real
// desktop; until then they play only when picked by name.
const EFFECTS = ['fade', 'zoom', 'slide', 'pixelate', 'wipe', 'blur'];

// Shared by every shader. `seed` (0..1) varies each transition. Lengths are in
// screen heights (`wsl_q`), so effects look the same at any resolution, and
// the noise is smooth: per-pixel or per-row randomness smears into streaks.
const DECLARATIONS = `
uniform float progress;
uniform vec2 size;
uniform float seed;

vec2 wsl_q(vec2 st) {
    return st * vec2(size.x / size.y, 1.0);
}

float wsl_hash(vec2 p) {
    p = fract(p * vec2(234.34, 435.345));
    p += dot(p, p + 34.23);
    return fract(p.x * p.y);
}

float wsl_noise(vec2 p) {
    vec2 i = floor(p);
    vec2 f = fract(p);
    vec2 u = f * f * (3.0 - 2.0 * f);
    return mix(mix(wsl_hash(i), wsl_hash(i + vec2(1.0, 0.0)), u.x),
        mix(wsl_hash(i + vec2(0.0, 1.0)), wsl_hash(i + vec2(1.0, 1.0)), u.x), u.y);
}

float wsl_fbm(vec2 p) {
    float v = 0.0;
    float a = 0.5;
    for (int i = 0; i < 4; i++) {
        v += a * wsl_noise(p);
        p = p * 2.03 + 17.1;
        a *= 0.5;
    }
    return v / 0.9375;
}

// Breaks up banding in soft glows.
float wsl_dither(vec2 st) {
    return (wsl_hash(st * size) - 0.5) / 255.0;
}`;

// Shatter: shard sites on a jittered grid, CELL screen heights apart.
const SHATTER_DECLARATIONS = `
const float CELL = 0.34;

vec2 wsl_site(vec2 cell) {
    vec2 j = vec2(wsl_hash(cell + seed * 13.0), wsl_hash(cell.yx + seed * 29.0 + 3.0));
    return (cell + 0.5 + (j - 0.5) * 0.8) * CELL;
}

// The shard (grid cell of the nearest site) that point y belongs to.
vec2 wsl_owner(vec2 y) {
    vec2 c0 = floor(y / CELL);
    vec2 best = c0;
    float bd = 1e9;
    for (int j = -1; j <= 1; j++) {
        for (int i = -1; i <= 1; i++) {
            vec2 c = c0 + vec2(float(i), float(j));
            vec2 d = wsl_site(c) - y;
            float dd = dot(d, d);
            if (dd < bd) {
                bd = dd;
                best = c;
            }
        }
    }
    return best;
}

// Distance from y to the edge of the shard in cell, in screen heights.
float wsl_border(vec2 y, vec2 cell) {
    vec2 a = wsl_site(cell);
    float b = 1e9;
    for (int j = -1; j <= 1; j++) {
        for (int i = -1; i <= 1; i++) {
            vec2 c = cell + vec2(float(i), float(j));
            if (c == cell)
                continue;
            vec2 o = wsl_site(c);
            b = min(b, dot(0.5 * (a + o) - y, normalize(o - a)));
        }
    }
    return b;
}`;

// Each snippet fades the old wallpaper out (premultiplied alpha) as progress
// goes from 0 to 1, revealing the new one underneath. Light added to the
// colour without alpha (glows, leaks) shows over the new wallpaper too.
const SHADERS = {
    pixelate: `
        float px = 1.0 + progress * progress * 96.0;
        vec2 uv = (floor(cogl_tex_coord0_in.st * size / px) + 0.5) * px / size;
        cogl_color_out = texture2D(cogl_sampler0, uv);
        cogl_color_out *= 1.0 - smoothstep(0.55, 1.0, progress);`,
    // A smooth diagonal edge. Per-row jitter here once smeared the soft edge
    // into horizontal streaks.
    wipe: `
        vec2 st = cogl_tex_coord0_in.st;
        float d = st.x * 0.75 + st.y * 0.25;
        float a = smoothstep(progress * 1.15 - 0.15, progress * 1.15, d);
        cogl_color_out *= a;`,
    // The Star Wars crawl: the wallpaper tilts back on a hinge at one edge
    // (bottom or top, by seed) and drifts away along its own plane, fading
    // into the distance. Projected here rather than with a Clutter rotation,
    // whose perspective is centred on the whole stage, not on each monitor.
    tilt: `
        vec2 st = cogl_tex_coord0_in.st;
        float aspect = size.x / size.y;
        bool down = seed < 0.5;
        float v = down ? st.y : 1.0 - st.y; // 0 at the hinge
        float th = radians(50.0) * progress;
        float k = tan(th);
        float D = 1.7; // eye distance, in screen heights
        float lam = (D + 0.5 * k) / max(D - (v - 0.5) * k, 1e-3);
        float t = (0.5 + lam * (v - 0.5)) / cos(th) - 0.3 * progress * progress;
        float s = lam * (st.x - 0.5) * aspect;
        vec2 uv = vec2(s / aspect + 0.5, down ? t : 1.0 - t);
        float aa = 1.5 * lam / size.y;
        float a = smoothstep(0.0, aa, t) * smoothstep(1.0, 1.0 - aa, t)
            * smoothstep(0.5 * aspect, 0.5 * aspect - aa, abs(s));
        a *= 1.0 - smoothstep(0.15, 1.0, t) * smoothstep(0.0, 0.8, progress);
        a *= 1.0 - smoothstep(0.6, 1.0, progress);
        cogl_color_out = texture2D(cogl_sampler0, uv) * a;`,
    // Smooth noise eats the wallpaper from two points; the image chars just
    // ahead of the front, and the front itself is a thin, dim ember.
    burn: `
        vec2 st = cogl_tex_coord0_in.st;
        vec2 q = wsl_q(st);
        float aspect = size.x / size.y;
        vec2 a = vec2(fract(seed * 7.13) * aspect, fract(seed * 3.71));
        vec2 b = vec2(fract(seed * 5.37 + 0.5) * aspect, fract(seed * 9.11 + 0.3));
        float reach = length(vec2(aspect, 1.0));
        float d = min(length(q - a), length(q - b) + 0.2) / reach;
        float f = d * 0.7 + wsl_fbm(q * 2.5 + seed * 50.0) * 0.35;
        // f is mostly 0.05..0.6, so the front sweeps just that range.
        float x = f - mix(0.03, 0.64, progress); // > 0: not burnt yet
        vec4 c = cogl_color_out;
        c.rgb *= mix(0.45, 1.0, smoothstep(0.0, 0.04, x));
        c *= smoothstep(0.0, 0.005, x) * (1.0 - smoothstep(0.9, 1.0, progress));
        float flicker = 0.7 + 0.3 * wsl_noise(q * 30.0 + progress * 4.0);
        float glow = exp(-abs(x - 0.002) * 320.0) * flicker;
        c.rgb += vec3(1.0, 0.45, 0.12) * glow * 0.5 + wsl_dither(st);
        cogl_color_out = c;`,
    // Frost creeps in from the edges as frosted glass: the image scatters a
    // few pixels, cools a little and gathers soft rime; then it thaws unevenly.
    freeze: `
        vec2 st = cogl_tex_coord0_in.st;
        vec2 q = wsl_q(st);
        float aspect = size.x / size.y;
        float edge = min(min(q.x, aspect - q.x), min(q.y, 1.0 - q.y));
        float f = edge + (wsl_fbm(q * 3.0 + seed * 40.0) - 0.5) * 0.3;
        float front = mix(-0.1, 0.75, clamp(progress / 0.65, 0.0, 1.0));
        float frost = 1.0 - smoothstep(front - 0.12, front, f);
        vec2 grain = vec2(wsl_noise(q * 220.0 + 1.7), wsl_noise(q * 220.0 + 8.3)) - 0.5;
        vec4 c = texture2D(cogl_sampler0, st + grain * 6.0 * frost / size);
        float lum = dot(c.rgb, vec3(0.299, 0.587, 0.114));
        c.rgb = mix(c.rgb, vec3(0.86, 0.93, 1.0) * (lum * 0.8 + 0.2 * c.a), 0.2 * frost);
        float rime = smoothstep(0.55, 0.85, wsl_fbm(q * 14.0 - seed * 20.0));
        c.rgb += vec3(0.92, 0.96, 1.0) * (rime * 0.12 + 0.03) * frost * c.a;
        float thaw = smoothstep(0.65, 1.0, progress);
        float a = clamp((1.0 - thaw) * 1.6 - wsl_fbm(q * 2.5 + 9.0) * 0.6, 0.0, 1.0);
        cogl_color_out = (c + wsl_dither(st)) * a;`,
    // Hairline cracks spread from an impact point, then the shards drift down
    // and apart a little and fade, each on its own schedule.
    shatter: {
        declarations: SHATTER_DECLARATIONS,
        code: `
        vec2 st = cogl_tex_coord0_in.st;
        float aspect = size.x / size.y;
        vec2 x = wsl_q(st);
        vec2 hit = vec2((0.3 + 0.4 * fract(seed * 7.7)) * aspect, 0.3 + 0.4 * fract(seed * 3.3));
        float reach = length(vec2(aspect, 1.0));
        float cracked = smoothstep(0.0, 0.3, progress) * reach * 1.1;
        float px = 1.0 / size.y;
        vec2 c0 = floor(x / CELL);
        vec4 outc = vec4(0.0);
        for (int j = -1; j <= 1; j++) {
            for (int i = -1; i <= 1; i++) {
                vec2 cell = c0 + vec2(float(i), float(j));
                vec2 site = wsl_site(cell);
                float h = 0.6 * length(site - hit) / reach + 0.4 * wsl_hash(cell + seed * 7.0 + 11.0);
                float t = clamp((progress - 0.25 - h * 0.35) / 0.4, 0.0, 1.0);
                float tt = t * t;
                vec2 away = normalize(site - hit + 1e-4);
                vec2 off = (away * 0.025 + vec2(0.0, 0.07)) * tt;
                float ang = (wsl_hash(cell + seed * 3.0) - 0.5) * 0.1 * tt;
                vec2 r = x - off - site;
                vec2 y = site + vec2(cos(ang) * r.x + sin(ang) * r.y, -sin(ang) * r.x + cos(ang) * r.y);
                // Off the screen there is nothing to show (and edge
                // pixels would smear).
                if (y.x < 0.0 || y.y < 0.0 || y.x > aspect || y.y > 1.0 || wsl_owner(y) != cell)
                    continue;
                float border = wsl_border(y, cell);
                float crack = (1.0 - smoothstep(0.5 * px, 1.5 * px, border))
                    * (1.0 - smoothstep(cracked - 0.1, cracked, length(y - hit)));
                vec4 c = texture2D(cogl_sampler0, y / vec2(aspect, 1.0));
                c.rgb *= 1.0 - 0.6 * crack;
                c.rgb += vec3(0.1) * (1.0 - smoothstep(1.5 * px, 3.0 * px, border)) * crack * c.a;
                c *= mix(1.0, smoothstep(0.0, px, border), step(0.001, t)) * (1.0 - smoothstep(0.35, 1.0, t));
                if (c.a > outc.a)
                    outc = c;
            }
        }
        cogl_color_out = outc;`,
    },
    // A soft circle opens from the middle; the old image shades slightly
    // towards its edge.
    iris: `
        vec2 st = cogl_tex_coord0_in.st;
        float aspect = size.x / size.y;
        float r = length((st - 0.5) * vec2(aspect, 1.0));
        float R = mix(-0.15, 0.5 * length(vec2(aspect, 1.0)) + 0.02, progress);
        vec4 c = cogl_color_out;
        c.rgb *= mix(0.82, 1.0, smoothstep(R, R + 0.35, r));
        cogl_color_out = (c + wsl_dither(st)) * smoothstep(R, R + 0.15, r);`,
    // One gentle refracting ring spreads from the middle, taking the old
    // image with it.
    ripple: `
        vec2 st = cogl_tex_coord0_in.st;
        float aspect = size.x / size.y;
        vec2 q = (st - 0.5) * vec2(aspect, 1.0);
        float r = length(q);
        float R = progress * (0.5 * length(vec2(aspect, 1.0)) + 0.2);
        float w = 0.09;
        float dr = r - R;
        float wave = sin(dr / w * 4.7) * exp(-dr * dr / (w * w));
        vec2 dir = q / max(r, 1e-4);
        vec4 c = texture2D(cogl_sampler0, st + dir * wave * 0.012 / vec2(aspect, 1.0));
        c.rgb += vec3(max(wave, 0.0) * 0.06) * c.a;
        cogl_color_out = c * smoothstep(R - 0.5 * w, R + 0.5 * w, r);`,
    // A calm Doom melt: smooth columns slide down at staggered times.
    melt: `
        vec2 st = cogl_tex_coord0_in.st;
        float x = wsl_q(st).x;
        float delay = 0.32 * wsl_noise(vec2(x * 3.0, seed * 10.0)) + 0.06 * wsl_noise(vec2(x * 14.0, seed * 10.0 + 5.0));
        float t = max(progress * 1.3 - delay, 0.0);
        float shift = t * t * 1.4;
        float sy = st.y - shift;
        vec4 c = texture2D(cogl_sampler0, vec2(st.x, sy));
        c.rgb *= 1.0 - 0.15 * smoothstep(0.0, 0.05, shift) * (1.0 - smoothstep(0.0, 0.06, sy));
        cogl_color_out = c * smoothstep(0.0, 2.0 / size.y, sy) * (1.0 - smoothstep(0.8, 1.0, progress));`,
    // A crossfade with a warm band of light sweeping across it.
    leak: `
        vec2 st = cogl_tex_coord0_in.st;
        vec2 q = wsl_q(st);
        vec2 dir = normalize(vec2(1.0, 0.4));
        float span = dot(vec2(size.x / size.y, 1.0), dir);
        float u = dot(q, dir) - mix(-0.5, span + 0.5, progress);
        float light = exp(-u * u / 0.16) * sin(3.14159 * progress);
        float tint = exp(-(u + 0.25) * (u + 0.25) / 0.05);
        vec4 c = cogl_color_out * (1.0 - smoothstep(0.2, 0.8, progress));
        c.rgb += (vec3(1.0, 0.62, 0.32) * light * 0.3 + vec3(1.0, 0.4, 0.45) * tint * light * 0.12) + wsl_dither(st);
        cogl_color_out = c;`,
};

// Shell.GLSLEffect caches the pipeline per GType, so each shader needs its own class.
const ShaderEffects = Object.fromEntries(Object.entries(SHADERS).map(([name, shader]) => [name,
    GObject.registerClass({GTypeName: `WallslaedTransition_${name}`},
        class extends Shell.GLSLEffect {
            vfunc_build_pipeline() {
                this.add_glsl_snippet(Cogl.SnippetHook.FRAGMENT,
                    DECLARATIONS + (shader.declarations ?? ''), shader.code ?? shader, false);
            }

            setup(width, height) {
                this._progress = this.get_uniform_location('progress');
                // Pixelate divides by the size; never let it be zero.
                this.set_uniform_float(this.get_uniform_location('size'), 2,
                    [Math.max(1, width), Math.max(1, height)]);
                // Compiled out of shaders that don't use it.
                const seed = this.get_uniform_location('seed');
                if (seed >= 0)
                    this.set_uniform_float(seed, 1, [Math.random()]);
                this.setProgress(0);
            }

            setProgress(p) {
                this.set_uniform_float(this._progress, 1, [p]);
                this.queue_repaint();
            }
        }),
]));

export class Transitions {
    constructor(settings, injections) {
        this._settings = settings;
        // Background manager (one per monitor) -> its outgoing wallpaper.
        this._running = new Map();
        // Monitors showing the same picture share one Background; remember
        // the last one so they all play the same effect.
        this._picked = null;

        const self = this;
        // Same as GNOME's own _swapBackgroundActor, with a choice of effect
        // instead of the fixed one-second crossfade.
        injections.overrideMethod(Background.BackgroundManager.prototype, '_swapBackgroundActor',
            original => function () {
                if (this._container !== Main.layoutManager._backgroundGroup)
                    return original.call(this);

                const oldActor = this.backgroundActor;
                this.backgroundActor = this._newBackgroundActor;
                this._newBackgroundActor = null;
                this.emit('changed');

                // A wallpaper that arrives mid-transition skips its own
                // effect: the one still running reveals the newest wallpaper,
                // so two effects never stack on one monitor.
                if (Main.layoutManager.screenTransition.visible || !oldActor.visible ||
                    self._running.has(this))
                    oldActor.destroy();
                else
                    self._run(this, oldActor);
            });
    }

    _pick(background) {
        if (this._picked?.background === background)
            return this._picked.name;
        let name = this._settings.get_string('transition');
        if (name === 'random')
            name = EFFECTS[Math.floor(Math.random() * EFFECTS.length)];
        this._picked = {background, name};
        return name;
    }

    _run(manager, actor) {
        const name = this._pick(manager.backgroundActor.content.background);
        const duration = this._settings.get_uint('transition-duration');
        if (name === 'none' || !St.Settings.get().enable_animations) {
            actor.destroy();
            return;
        }

        this._running.set(manager, actor);
        actor.connect('destroy', () => {
            if (this._running.get(manager) === actor)
                this._running.delete(manager);
        });
        const done = () => actor.destroy();

        // The outgoing wallpaper stays on top while it fades. Let clicks reach
        // the new one underneath, so a right-click never opens a menu that is
        // about to be destroyed with its wallpaper.
        actor.reactive = false;
        actor._backgroundMenu?.close(BoxPointer.PopupAnimation.NONE);

        switch (name) {
        case 'zoom':
            actor.set_pivot_point(0.5, 0.5);
            actor.ease({
                scale_x: 1.12, scale_y: 1.12, opacity: 0, duration,
                mode: Clutter.AnimationMode.EASE_IN_QUAD, onComplete: done,
            });
            return;
        case 'slide':
            actor.ease({
                translation_x: -actor.width, opacity: 64, duration,
                mode: Clutter.AnimationMode.EASE_IN_OUT_CUBIC, onComplete: done,
            });
            return;
        case 'blur':
            this._animate(actor, duration,
                new Shell.BlurEffect({mode: Shell.BlurMode.ACTOR, radius: 0, brightness: 1}),
                (effect, p) => {
                    effect.radius = p * 64;
                    actor.opacity = 255 * (1 - p);
                });
            return;
        default:
            if (ShaderEffects[name]) {
                const effect = new ShaderEffects[name]();
                this._animate(actor, duration, effect, (e, p) => e.setProgress(p),
                    () => effect.setup(actor.width, actor.height));
                return;
            }
            actor.ease({
                opacity: 0, duration,
                mode: Clutter.AnimationMode.EASE_OUT_QUAD, onComplete: done,
            });
        }
    }

    // `init` runs once the effect is attached, before the first step.
    _animate(actor, duration, effect, step, init) {
        actor.add_effect(effect);
        init?.();
        const timeline = new Clutter.Timeline({actor, duration});
        timeline.set_progress_mode(Clutter.AnimationMode.EASE_IN_OUT_QUAD);
        timeline.connect('new-frame', () => step(effect, timeline.get_progress()));
        timeline.connect('completed', () => actor.destroy());
        actor.connect('destroy', () => timeline.stop());
        step(effect, 0);
        timeline.start();
    }

    destroy() {
        // Finish anything mid-transition right away.
        for (const actor of this._running.values())
            actor.destroy();
        this._running.clear();
        this._picked = null;
    }
}
