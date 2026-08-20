// Playback scaffolding, shared by generated modules.
//
// This is the part of `mount` that has nothing to do with what an animation
// contains: the frame clock, the loop policy, the event list and the public
// API. It takes `apply` and calls it once per frame — one indirection for the
// whole animation, where the interpreter pays one per bound property.
//
// Everything else a generated module does is generated.

// Frames crossing the API are 0-based within `[ip, op)`, the way lottie-web
// counts them: `goToAndStop(n, true)` renders `ip + n` there, and every caller
// in this repo pairs the two calls as though they named the same picture. The
// clock below stays absolute because that is what `apply` takes, so `ip` is
// added on the way in and taken off on the way out. It only shows on an
// animation whose `ip` is not 0 — `lf20_tWzLYe` starts at 3.0000001 and was
// being compared three frames out of step, which read as a rendering bug.
// `adopt` says the `<svg>` was found in the container rather than built: the
// player then leaves it there on `destroy()` — it was never this module's to
// clear, whether the page served it or a previous mount did.
export function player(container, svg, markup, apply, fr, ip, op, opt, adopt) {
  const span = op - ip || 1;
  let raf = 0;
  let prev = 0;
  let frame = ip;
  let dir = 1;
  let rate = 1;
  let loops = 0;
  let loop = opt.loop === undefined ? true : opt.loop;
  const subs = {};

  const fire = (name, arg) => {
    const l = subs[name];
    if (l) for (let i = 0; i < l.length; i++) l[i](arg);
  };

  function tick(ts) {
    raf = requestAnimationFrame(tick);
    const dt = prev ? (ts - prev) / 1000 : 0;
    prev = ts;
    frame += dt * fr * rate * dir;
    if (frame >= op || frame < ip) {
      if (loop === true || loops + 1 < loop) {
        loops++;
        frame = frame >= op
          ? ip + ((frame - ip) % span)
          : op - ((ip - frame) % span);
        fire('loop', loops);
      } else {
        frame = dir > 0 ? op - 1e-4 : ip;
        halt();
        apply(frame);
        fire('complete');
        return;
      }
    }
    apply(frame);
    fire('frame', frame - ip);
  }

  function halt() {
    if (raf) cancelAnimationFrame(raf);
    raf = 0;
    prev = 0;
  }

  const p = {
    svg,
    markup,
    totalFrames: span,
    frameRate: fr,
    duration: span / fr,
    get currentFrame() { return frame - ip; },
    get isPlaying() { return !!raf; },
    get loop() { return loop; },
    set loop(v) { loop = v; loops = 0; },
    get speed() { return rate; },
    set speed(v) { rate = v; },
    get direction() { return dir; },
    set direction(v) { dir = v < 0 ? -1 : 1; },
    play() {
      if (!raf) { prev = 0; raf = requestAnimationFrame(tick); }
      return p;
    },
    pause() { halt(); return p; },
    stop() { halt(); frame = ip; apply(ip); return p; },
    seek(f) { frame = ip + f; apply(frame); return p; },
    goToFrame(f) { halt(); frame = ip + f; apply(frame); return p; },
    goToAndStop(f) { return p.goToFrame(f); },
    goToAndPlay(f) { frame = ip + f; apply(frame); return p.play(); },
    on(name, fn) { (subs[name] || (subs[name] = [])).push(fn); return p; },
    off(name, fn) {
      const l = subs[name];
      if (l) { const i = l.indexOf(fn); if (i >= 0) l.splice(i, 1); }
      return p;
    },
    destroy() {
      halt();
      if (!adopt) container.innerHTML = '';
    },
  };

  apply(ip);
  // `autoplay` defaults to 'auto': play, unless the OS asks for reduced motion.
  const auto = opt.autoplay;
  if (auto !== false && (auto === true || !slowed())) p.play();
  return p;
}

function slowed() {
  return typeof matchMedia === 'function'
    && matchMedia('(prefers-reduced-motion: reduce)').matches;
}
