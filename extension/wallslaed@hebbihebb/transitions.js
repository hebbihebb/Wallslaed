// Transition effects between wallpapers. Effects are attached to the outgoing
// background only for the length of the transition and removed afterwards, so
// they cost nothing while the desktop is idle.

import Clutter from 'gi://Clutter';
import Cogl from 'gi://Cogl';
import GObject from 'gi://GObject';
import Shell from 'gi://Shell';
import St from 'gi://St';

import * as Background from 'resource:///org/gnome/shell/ui/background.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';

const EFFECTS = ['fade', 'zoom', 'slide', 'dissolve', 'pixelate', 'wipe', 'blur'];

const DECLARATIONS = `
uniform float progress;
uniform vec2 size;
float hash(vec2 p) {
    return fract(sin(dot(p, vec2(12.9898, 78.233))) * 43758.5453);
}`;

// Each snippet fades the old wallpaper out (premultiplied alpha) as progress
// goes from 0 to 1, revealing the new one underneath.
const SHADERS = {
    dissolve: `
        vec2 cell = floor(cogl_tex_coord0_in.st * size / 6.0);
        float n = hash(cell);
        float a = smoothstep(progress * 1.1 - 0.1, progress * 1.1, n);
        cogl_color_out *= a;`,
    pixelate: `
        float px = 1.0 + progress * progress * 96.0;
        vec2 uv = (floor(cogl_tex_coord0_in.st * size / px) + 0.5) * px / size;
        cogl_color_out = texture2D(cogl_sampler0, uv);
        cogl_color_out *= 1.0 - smoothstep(0.55, 1.0, progress);`,
    wipe: `
        vec2 st = cogl_tex_coord0_in.st;
        float d = (st.x * 0.75 + st.y * 0.25) + (hash(vec2(floor(st.y * size.y / 3.0), 0.0)) - 0.5) * 0.02;
        float a = smoothstep(progress * 1.15 - 0.15, progress * 1.15, d);
        cogl_color_out *= a;`,
};

// Shell.GLSLEffect caches the pipeline per GType, so each shader needs its own class.
const ShaderEffects = Object.fromEntries(Object.entries(SHADERS).map(([name, code]) => [name,
    GObject.registerClass({GTypeName: `WallslaedTransition_${name}`},
        class extends Shell.GLSLEffect {
            vfunc_build_pipeline() {
                this.add_glsl_snippet(Cogl.SnippetHook.FRAGMENT, DECLARATIONS, code, false);
            }

            setup(width, height) {
                this._progress = this.get_uniform_location('progress');
                // Pixelate divides by the size; never let it be zero.
                this.set_uniform_float(this.get_uniform_location('size'), 2,
                    [Math.max(1, width), Math.max(1, height)]);
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
        this._running = new Set();

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

                if (Main.layoutManager.screenTransition.visible || !oldActor.visible)
                    oldActor.destroy();
                else
                    self._run(oldActor);
            });
    }

    _pick() {
        let name = this._settings.get_string('transition');
        if (name === 'random')
            name = EFFECTS[Math.floor(Math.random() * EFFECTS.length)];
        return name;
    }

    _run(actor) {
        const name = this._pick();
        const duration = this._settings.get_uint('transition-duration');
        if (name === 'none' || !St.Settings.get().enable_animations) {
            actor.destroy();
            return;
        }

        this._running.add(actor);
        actor.connect('destroy', () => this._running.delete(actor));
        const done = () => actor.destroy();

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
        case 'dissolve':
        case 'pixelate':
        case 'wipe': {
            const effect = new ShaderEffects[name]();
            this._animate(actor, duration, effect, (e, p) => e.setProgress(p),
                () => effect.setup(actor.width, actor.height));
            return;
        }
        default:
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
        for (const actor of this._running)
            actor.destroy();
        this._running.clear();
    }
}
