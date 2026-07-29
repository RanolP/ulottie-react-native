//! Emitting an animation as code rather than as data.
//!
//! The interpreter reads a payload and dispatches through a binder table, which
//! means every self-contained module carries the machinery for every capability
//! — `bouncy_ball` animates one transform and ships forty runtime symbols to do
//! it, 93% of the file. That is backwards for an ahead-of-time compiler: what is
//! known at compile time should be spent at compile time.
//!
//! So this walks the planned scene and writes the animation out as straight-line
//! JavaScript. There is no payload, no binder table, no `resolve`, and no
//! closure per property; a frame is one call into generated code. What is left
//! shared is the part that genuinely does not vary — the frame clock and the
//! public API in `runtime/play.js`, plus whatever formatting helpers the
//! animation actually reaches.
//!
//! It also folds. A rotation the planner proved constant at zero collapses the
//! transform to a diagonal matrix with no `cos`, no `sin` and no four-term
//! multiply — something a data-driven runtime cannot do, because it does not
//! learn the value until it runs.
//!
//! Coverage is deliberately partial. [`try_emit`] returns `None` for anything it
//! cannot express, and the caller falls back to the interpreter, so a module is
//! only ever built one way and never pays for the path it did not take.

use std::fmt::Write;

use crate::scene::prop::{Anim, AnimKind, Prop};
use crate::scene::svg::{n as fmt_num, q, FlatPath};
use crate::scene::flat::RECORD_DEFAULTS;
use crate::scene::{op, Arg, Binding, Effect, Scene};

/// A generated value: either a number the compiler knows, or JS that computes
/// one. Keeping the two apart is what makes constant folding fall out.
#[derive(Clone, Debug)]
enum Val {
    Lit(f64),
    Expr(String),
}

impl Val {
    fn js(&self) -> String {
        match self {
            Val::Lit(v) => fmt_num(*v),
            Val::Expr(s) => s.clone(),
        }
    }

    fn lit(&self) -> Option<f64> {
        match self {
            Val::Lit(v) => Some(*v),
            _ => None,
        }
    }
}

fn mul(a: &Val, b: &Val) -> Val {
    match (a.lit(), b.lit()) {
        (Some(x), Some(y)) => Val::Lit(x * y),
        (Some(x), _) if x == 0.0 => Val::Lit(0.0),
        (_, Some(y)) if y == 0.0 => Val::Lit(0.0),
        (Some(x), _) if x == 1.0 => b.clone(),
        (_, Some(y)) if y == 1.0 => a.clone(),
        _ => Val::Expr(format!("{}*{}", paren(a), paren(b))),
    }
}

fn sub(a: &Val, b: &Val) -> Val {
    match (a.lit(), b.lit()) {
        (Some(x), Some(y)) => Val::Lit(x - y),
        (_, Some(y)) if y == 0.0 => a.clone(),
        _ => Val::Expr(format!("{}-{}", a.js(), paren(b))),
    }
}

fn add(a: &Val, b: &Val) -> Val {
    match (a.lit(), b.lit()) {
        (Some(x), Some(y)) => Val::Lit(x + y),
        (Some(x), _) if x == 0.0 => b.clone(),
        (_, Some(y)) if y == 0.0 => a.clone(),
        _ => Val::Expr(format!("{}+{}", a.js(), paren(b))),
    }
}

/// Wrap anything that is not already a single term.
fn paren(v: &Val) -> String {
    match v {
        Val::Lit(x) if *x < 0.0 => format!("({})", fmt_num(*x)),
        Val::Lit(x) => fmt_num(*x),
        Val::Expr(s) if s.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.') => s.clone(),
        Val::Expr(s) => format!("({s})"),
    }
}

// ---------------------------------------------------------------------------
// Coverage
// ---------------------------------------------------------------------------

/// Ops this generator can write out. The rest keep the interpreter.
fn op_supported(o: u8) -> bool {
    matches!(
        o,
        op::TRANSFORM
            | op::TRANSLATE
            | op::OPACITY
            | op::DISPLAY
            | op::FILL
            | op::STROKE
            | op::SHAPE
            | op::RECT
            | op::ELLIPSE
            | op::GRADIENT
            | op::LAYER_TX
            | op::LAYER_OP
    )
}

/// Properties it can evaluate: constants, and keyframed scalars and vectors
/// without spatial tangents. Paths, expressions and motion paths do not fit in
/// straight-line code without dragging their runtime in anyway.
fn prop_supported(p: &Prop) -> bool {
    match p {
        Prop::Scalar(_) | Prop::Vector(_) | Prop::Path(_) => true,
        // An expression is a handle handed to the engine. The engine no longer
        // knows where handles come from, so emitting one is enough.
        Prop::Expr { fallback, .. } => fallback.as_deref().is_none_or(prop_supported),
        // Explicit segment ends (legacy `e`) are rare and would double every
        // segment's emitted values; not worth the code until a fixture needs it.
        Prop::Anim(a) => a.end.is_none() && a.end_paths.is_none(),
    }
}

fn args_supported(args: &[Arg]) -> bool {
    args.iter().all(|a| match a {
        Arg::Prop(p) => prop_supported(p),
        Arg::List(items) => args_supported(items),
        _ => true,
    })
}

/// Whether the whole scene is expressible. Anything structural the generator
/// does not model yet — precomps, templates, expression records, per-binding
/// clocks or visibility gates — sends the module back to the interpreter.
fn scene_supported(scene: &Scene) -> bool {
    let d = &scene.data;
    if std::env::var("ULOTTIE_WHY").is_ok() {
        for (name, ok) in [
            ("assets", d.assets.is_empty()),
            ("uses", d.uses.is_empty()),
            ("remaps", d.remaps.iter().all(|r| r.is_none())),
            ("bindings", !d.b.is_empty()),
            ("ops", d.b.iter().all(|b| op_supported(b.op))),
            ("args", d.b.iter().all(|b| args_supported(&b.args))),
        ] {
            if !ok { eprintln!("codegen: falls back on {name}"); }
        }
        for b in &d.b {
            if !args_supported(&b.args) {
                eprintln!("  op {} has an unsupported arg", b.op);
            }
        }
    }
    // Instancing is the one structural thing left on the interpreter. Nothing
    // in the corpus reaches it by default — `Instancing::Auto` inlines a
    // precomp whose instances carry their own clocks — but `--instance-precomps`
    // does, and that build still compiles, just the other way.
    d.assets.is_empty()
        && d.uses.is_empty()
        && d.remaps.iter().all(|r| r.as_ref().is_none_or(prop_supported))
        && !d.b.is_empty()
        && d.b.iter().all(|b| op_supported(b.op) && args_supported(&b.args))
}

// ---------------------------------------------------------------------------
// Generation
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Builder {
    /// Statements for the body of `apply`.
    body: String,
    /// Easing handles this animation reaches, in emission order.
    easings: Vec<[f64; 4]>,
    /// Next temporary name.
    tmp: usize,
    /// Next arc-length table.
    tabs: usize,
    /// Constant path objects, hoisted to module scope.
    paths: Vec<String>,
    /// Declarations for the body of `init`, after the engine is installed:
    /// property handles, records, scopes.
    init: String,
    /// Next handle name.
    handles: usize,
    /// Next keyframe-column table.
    tables: usize,
    /// Emitted handle text → its name, so identical properties share one.
    pool: std::collections::HashMap<String, String>,
    /// Helper names the generated code calls.
    needs: Vec<&'static str>,
    /// Module-scope declarations: arc-length tables, built once at load.
    pre: String,
    /// Declarations for the body of `init`: scratch buffers.
    decls: Vec<String>,
}

impl Builder {
    fn need(&mut self, name: &'static str) {
        if !self.needs.contains(&name) {
            self.needs.push(name);
        }
    }

    fn name(&mut self) -> String {
        self.tmp += 1;
        format!("v{}", self.tmp)
    }

    /// One reusable path object per geometry binding, so the generators write
    /// into it instead of allocating every frame.
    fn scratch(&mut self, bi: usize) -> String {
        let name = format!("k{bi}");
        if !self.decls.iter().any(|d| d.starts_with(&format!("{name}="))) {
            self.decls.push(format!("{name}={{v:[],i:null,o:null,c:1}}"));
        }
        name
    }

    /// Index of an easing in this module's table, interning as it goes. Index 0
    /// is linear and never reaches the solver.
    fn ease(&mut self, e: [f64; 4]) -> Option<usize> {
        if e == [0.0, 0.0, 1.0, 1.0] {
            return None;
        }
        if let Some(i) = self.easings.iter().position(|x| *x == e) {
            return Some(i);
        }
        self.easings.push(e);
        Some(self.easings.len() - 1)
    }

    /// A constant path, hoisted to module scope as an object literal. Two
    /// identical paths share one — a keyframed shape usually returns to the
    /// same outline, and `lerpPath` compares by length anyway.
    fn path_const(&mut self, p: &FlatPath) -> String {
        let poly = p.i.iter().chain(p.o.iter()).all(|x| *x == 0.0);
        let col = |v: &Vec<f64>| {
            v.iter().map(|x| fmt_num(q(*x))).collect::<Vec<_>>().join(",")
        };
        let body = if poly {
            format!("{{v:[{}],i:null,o:null,c:{}}}", col(&p.v), p.c as u8)
        } else {
            format!(
                "{{v:[{}],i:[{}],o:[{}],c:{}}}",
                col(&p.v),
                col(&p.i),
                col(&p.o),
                p.c as u8
            )
        };
        if let Some(i) = self.paths.iter().position(|x| *x == body) {
            return format!("Q{i}");
        }
        self.paths.push(body);
        format!("Q{}", self.paths.len() - 1)
    }

    /// A property that evaluates to a path object, as a JS expression.
    fn path_prop(&mut self, p: &Prop, easings: &[[f64; 4]]) -> Option<String> {
        match p {
            Prop::Path(fp) => Some(self.path_const(fp)),
            Prop::Expr { .. } => {
                let h = self.handle(p, easings);
                let name = self.name();
                writeln!(self.body, "const {name}={h}(f);").unwrap();
                Some(name)
            }
            Prop::Anim(a) if a.kind == AnimKind::Path => {
                self.need("lerpPath");
                let n = a.t.len();
                let keys: Vec<String> =
                    a.paths.iter().map(|fp| self.path_const(fp)).collect();
                let name = self.name();
                writeln!(self.body, "let {name};").unwrap();
                writeln!(self.body, "if(f<={}){{{name}={}}}", fmt_num(q(a.t[0])), keys[0]).unwrap();
                writeln!(
                    self.body,
                    "else if(f>={}){{{name}={}}}",
                    fmt_num(q(a.t[n - 1])),
                    keys[n - 1]
                )
                .unwrap();
                for i in 0..n - 1 {
                    let (ta, tb) = (q(a.t[i]), q(a.t[i + 1]));
                    let span = tb - ta;
                    let held =
                        a.hold.as_ref().is_some_and(|h| h.get(i).copied().unwrap_or(0) == 1);
                    if span == 0.0 || held {
                        writeln!(self.body, "else if(f<{}){{{name}={}}}", fmt_num(tb), keys[i])
                            .unwrap();
                        continue;
                    }
                    let ez = a
                        .ez
                        .as_ref()
                        .and_then(|z| z.get(i).copied())
                        .and_then(|k| easings.get(k as usize).copied())
                        .and_then(|e| self.ease(e));
                    let u = match ez {
                        Some(k) => {
                            self.need("EASE");
                            format!("EASE(Z{k},(f-{})/{})", fmt_num(ta), fmt_num(span))
                        }
                        None => format!("(f-{})/{}", fmt_num(ta), fmt_num(span)),
                    };
                    writeln!(
                        self.body,
                        "else if(f<{}){{{name}=lerpPath({},{},{u})}}",
                        fmt_num(tb),
                        keys[i],
                        keys[i + 1]
                    )
                    .unwrap();
                }
                Some(name)
            }
            _ => None,
        }
    }

    /// A property too large to unroll: its columns as a literal, sampled by
    /// `kfEval`. The scratch buffer is per property, so this allocates nothing
    /// per frame either.
    fn anim_table(&mut self, a: &Anim, easings: &[[f64; 4]], d: usize) -> Vec<Val> {
        self.need("kfEval");
        let lit = self.kf_columns(a, easings);
        let key = format!("K{}", self.tables);
        let out = format!("q{}", self.tables);
        self.tables += 1;
        writeln!(self.pre, "const {key}={lit};").unwrap();
        self.decls.push(format!("{out}=new Array({d})"));
        let name = self.name();
        writeln!(self.body, "const {name}=kfEval({key},f,{out});").unwrap();
        if d == 1 {
            vec![Val::Expr(name)]
        } else {
            (0..d).map(|i| Val::Expr(format!("{name}[{i}]"))).collect()
        }
    }

    /// `{t,v,d,kind,z,h}` — the shape `kfEval` reads.
    fn kf_columns(&mut self, a: &Anim, easings: &[[f64; 4]]) -> String {
        let t: Vec<String> = a.t.iter().map(|x| fmt_num(q(*x))).collect();
        let v = if a.kind == AnimKind::Path {
            a.paths.iter().map(|p| self.path_const(p)).collect::<Vec<_>>().join(",")
        } else {
            a.v.iter().map(|x| fmt_num(q(*x))).collect::<Vec<_>>().join(",")
        };
        let mut out = format!(
            "{{t:[{}],v:[{v}],d:{},kind:{}",
            t.join(","),
            a.dim.max(1),
            a.kind as u8
        );
        // Easing handles inline, one per segment, 0 where the segment is linear.
        if let Some(z) = &a.ez {
            let items: Vec<String> = z
                .iter()
                .map(|k| match easings.get(*k as usize) {
                    Some(e) if *e != [0.0, 0.0, 1.0, 1.0] => format!(
                        "[{},{},{},{}]",
                        fmt_num(e[0]), fmt_num(e[1]), fmt_num(e[2]), fmt_num(e[3])
                    ),
                    _ => "0".into(),
                })
                .collect();
            if items.iter().any(|i| i != "0") {
                self.need("EASE");
                out.push_str(&format!(",z:[{}]", items.join(",")));
            }
        }
        if let Some(h) = &a.hold {
            if h.iter().any(|x| *x == 1) {
                let items: Vec<String> = h.iter().map(|x| x.to_string()).collect();
                out.push_str(&format!(",h:[{}]", items.join(",")));
            }
        }
        // Spatial tangents, per segment. Dropping these was silent — the
        // columns still evaluated, just along a straight line — and it is what
        // made `starfish`'s shipped build disagree with lottie-web while the
        // extern build, which reads the same tangents off the stream, did not.
        // `anim`'s unrolled form never had the gap; only the columns did.
        if let (Some(to), Some(ti)) = (&a.to, &a.ti) {
            if to.iter().chain(ti).any(|x| q(*x) != 0.0) {
                self.need("spBuild");
                self.need("spSample");
                let col = |v: &Vec<f64>| {
                    v.iter().map(|x| fmt_num(q(*x))).collect::<Vec<_>>().join(",")
                };
                out.push_str(&format!(",to:[{}],ti:[{}]", col(to), col(ti)));
            }
        }
        out.push('}');
        out
    }

    /// Emit a property as a **handle**: a named evaluator carrying whatever
    /// the expression engine needs hung off it. This is the same shape
    /// `rec.js` materializes from the stream, so the engine cannot tell the two
    /// apart — which is the whole point of the boundary.
    fn handle(&mut self, p: &Prop, easings: &[[f64; 4]]) -> String {
        let name = format!("H{}", self.handles);
        self.handles += 1;
        // This handle's own declaration is built aside; anything nested — an
        // expression's value source — appends to `init` as it is built and is
        // pooled in its own right. Building both in one buffer meant a pool hit
        // on the outer discarded a declaration the pool still pointed at.
        let mut decl = String::new();
        match p {
            Prop::Scalar(v) => {
                writeln!(decl, "const {name}=()=>{};", fmt_num(q(*v))).unwrap();
            }
            Prop::Vector(v) => {
                let items: Vec<String> = v.iter().map(|x| fmt_num(q(*x))).collect();
                // Hoisted, so reading it does not allocate on every frame.
                writeln!(decl, "const {name}_v=[{}],{name}=()=>{name}_v;", items.join(","))
                    .unwrap();
            }
            Prop::Path(fp) => {
                let c = self.path_const(fp);
                writeln!(decl, "const {name}=()=>{c};{name}.pathv={c};").unwrap();
            }
            Prop::Expr { id, fallback, layer } => {
                let src = match fallback.as_deref() {
                    Some(f) => self.handle(f, easings),
                    None => "null".to_string(),
                };
                let l = match layer {
                    Some(l) => format!("{l}"),
                    None => "undefined".to_string(),
                };
                writeln!(decl, "const {name}=ctx.expr({{x:{id},src:{src},l:{l}}});").unwrap();
            }
            Prop::Anim(a) => {
                // Columns, never an unrolled body. A handle has to carry its
                // keyframes as data anyway — `thisProperty.key(n)` reads them —
                // so unrolling as well wrote every value into the module twice.
                let lit = self.kf_columns(a, easings);
                let d = a.dim.max(1);
                self.need("kfEval");
                writeln!(
                    decl,
                    "const {name}_k={lit},{name}_o=new Array({d}),{name}=(f)=>kfEval({name}_k,f,{name}_o);{name}.kf={name}_k;"
                )
                .unwrap();
            }
        }
        // Fold this handle away if an identical one was already emitted. The
        // name is part of the text, so compare with it normalised out.
        let text = decl.replace(&name, "@");
        if let Some(prior) = self.pool.get(&text) {
            self.handles -= 1;
            return prior.clone();
        }
        self.init.push_str(&decl);
        self.pool.insert(text, name.clone());
        name
    }

    /// Emit a property, returning one `Val` per component.
    ///
    /// A constant becomes literals, which then fold into whatever consumes them.
    /// A keyframed property becomes an if-chain over its segments, assigning to
    /// temporaries — one branch per segment, no search and no interpolator call.
    fn prop(&mut self, p: &Prop, dim: usize, easings: &[[f64; 4]]) -> Vec<Val> {
        match p {
            Prop::Scalar(v) => vec![Val::Lit(q(*v)); dim.max(1)],
            Prop::Vector(v) => (0..dim.max(v.len()))
                .map(|i| Val::Lit(q(v.get(i).copied().unwrap_or(0.0))))
                .collect(),
            Prop::Anim(a) => self.anim(a, easings),
            // An expression is opaque: emit its handle once, call it per frame.
            Prop::Expr { .. } => {
                let h = self.handle(p, easings);
                let name = self.name();
                writeln!(self.body, "const {name}={h}(f);").unwrap();
                if dim.max(1) == 1 {
                    vec![Val::Expr(name)]
                } else {
                    (0..dim.max(1)).map(|i| Val::Expr(format!("{name}[{i}]"))).collect()
                }
            }
            // Filtered out by `prop_supported`.
            Prop::Path(_) => vec![Val::Lit(0.0); dim.max(1)],
        }
    }

    /// Above this many segments, a property ships as columns and a call rather
    /// than as an unrolled if-chain. Unrolling is faster and costs one branch
    /// per segment; past a handful that trade stops paying, and on `ripple` —
    /// 230 bindings — unrolling everything produced a 255 KB module against the
    /// interpreter's 52.
    const UNROLL_MAX: usize = 12;

    fn anim(&mut self, a: &Anim, easings: &[[f64; 4]]) -> Vec<Val> {
        let d = a.dim.max(1);
        let n = a.t.len();
        if n - 1 > Self::UNROLL_MAX && a.to.is_none() && a.kind != AnimKind::Path {
            return self.anim_table(a, easings, d);
        }
        let names: Vec<String> = (0..d).map(|_| self.name()).collect();
        let at = |k: usize, c: usize| q(a.v.get(k * d + c).copied().unwrap_or(0.0));

        writeln!(self.body, "let {};", names.join(",")).unwrap();

        // Before the first key and after the last, a property holds its end
        // values — the same clamp the interpreter does, minus the comparisons.
        let first: Vec<String> = (0..d).map(|c| fmt_num(at(0, c))).collect();
        let last: Vec<String> = (0..d).map(|c| fmt_num(at(n - 1, c))).collect();
        let t0 = fmt_num(q(a.t[0]));
        let tn = fmt_num(q(a.t[n - 1]));

        let assign = |names: &[String], vals: &[String]| {
            names
                .iter()
                .zip(vals)
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(";")
        };

        writeln!(self.body, "if(f<={t0}){{{}}}", assign(&names, &first)).unwrap();
        writeln!(self.body, "else if(f>={tn}){{{}}}", assign(&names, &last)).unwrap();

        for i in 0..n - 1 {
            let (ta, tb) = (q(a.t[i]), q(a.t[i + 1]));
            let span = tb - ta;
            let held = a.hold.as_ref().is_some_and(|h| h.get(i).copied().unwrap_or(0) == 1);
            let start: Vec<String> = (0..d).map(|c| fmt_num(at(i, c))).collect();

            if span == 0.0 || held {
                // A held segment keeps its start value for the whole span.
                writeln!(self.body, "else if(f<{}){{{}}}", fmt_num(tb), assign(&names, &start))
                    .unwrap();
                continue;
            }

            let ez = a
                .ez
                .as_ref()
                .and_then(|z| z.get(i).copied())
                .and_then(|k| easings.get(k as usize).copied())
                .and_then(|e| self.ease(e));

            // Spatial tangents bend the segment, and the path is sampled by
            // arc length rather than interpolated straight. The endpoints are
            // constants here, so the table is built when the module loads —
            // the interpreter has to defer it to the segment's first visit.
            let spatial = a.to.as_ref().zip(a.ti.as_ref()).and_then(|(to, ti)| {
                let g = |v: &Vec<f64>, c: usize| q(v.get(i * d + c).copied().unwrap_or(0.0));
                let any = (0..d).any(|c| g(to, c) != 0.0 || g(ti, c) != 0.0);
                any.then(|| {
                    let arr = |v: &Vec<f64>| {
                        (0..d).map(|c| fmt_num(g(v, c))).collect::<Vec<_>>().join(",")
                    };
                    let start = (0..d).map(|c| fmt_num(at(i, c))).collect::<Vec<_>>().join(",");
                    let end = (0..d).map(|c| fmt_num(at(i + 1, c))).collect::<Vec<_>>().join(",");
                    (format!("[{start}],[{end}],[{}],[{}],{d}", arr(to), arr(ti)), d)
                })
            });

            let u = match ez {
                Some(k) => {
                    // The solver is the runtime's, called with handles hoisted
                    // to module scope — an inline array literal would allocate
                    // one per frame per eased segment.
                    self.need("EASE");
                    format!("EASE(Z{k},(f-{})/{})", fmt_num(ta), fmt_num(span))
                }
                None => format!("(f-{})/{}", fmt_num(ta), fmt_num(span)),
            };
            let mut parts = vec![format!("const u={u}")];
            if let Some((args, dim)) = spatial {
                self.need("spBuild");
                self.need("spSample");
                let tab = format!("P{}", self.tabs);
                let out = format!("o{}", self.tabs);
                self.tabs += 1;
                writeln!(self.pre, "const {tab}=spBuild({args});").unwrap();
                self.decls.push(format!("{out}=new Array({dim})"));
                parts.push(format!("spSample({tab},u,{out})"));
                for c in 0..dim {
                    parts.push(format!("{}={out}[{c}]", names[c]));
                }
            } else {
                for c in 0..d {
                    let (va, vb) = (at(i, c), at(i + 1, c));
                    if va == vb {
                        parts.push(format!("{}={}", names[c], fmt_num(va)));
                    } else {
                        parts.push(format!(
                            "{}={}+{}*u",
                            names[c],
                            fmt_num(va),
                            fmt_num(q(vb - va))
                        ));
                    }
                }
            }
            writeln!(self.body, "else if(f<{}){{{}}}", fmt_num(tb), parts.join(";")).unwrap();
        }
        // The final segment's upper bound is the `f>=tn` clamp above, so the
        // chain needs no trailing branch beyond the last one written.
        names.into_iter().map(Val::Expr).collect()
    }

    /// `el.setAttribute(name, value)` with the change check inlined — one
    /// comparison against a dedicated slot, where the interpreter allocates a
    /// closure per attribute to hold the same state.
    fn write(&mut self, el: usize, attr: &str, value: &str, slot: &str) {
        writeln!(
            self.body,
            "{{const w={value};if(w!=={slot}){{{slot}=w;e{el}.setAttribute('{attr}',w)}}}}"
        )
        .unwrap();
    }
}

/// Try to write `scene` as code. `None` means something in it needs the
/// interpreter.
pub fn try_emit(scene: &Scene) -> Option<Generated> {
    if !scene_supported(scene) {
        return None;
    }
    let mut g = Builder::default();
    let easings = scene.data.easings.clone();
    let mut slots: Vec<String> = Vec::new();
    let mut els: Vec<u32> = Vec::new();

    // Precomp clocks. Every field of a timeline row is a compile-time
    // constant, so the whole table unrolls: slot 0 is the composition clock and
    // slot i+1 is one shifted, optionally-looping local time. Named locals, in
    // planner order, which already puts a parent before its children.
    for (i, t) in scene.data.timelines.iter().enumerate() {
        let parent = t[0] as usize;
        let src = if parent == 0 { "f".to_string() } else { format!("t{parent}") };

        // A precomp with time remap takes its clock from a property of the
        // parent's time rather than from `parent - offset`, and neither the
        // offset nor the loop applies. Lottie stores it in seconds.
        if let Some(Some(p)) = scene.data.remaps.get(i) {
            let h = g.handle(p, &easings);
            writeln!(
                g.body,
                "const t{}={h}({src})*{};",
                i + 1,
                fmt_num(q(scene.data.fr))
            )
            .ok()?;
            continue;
        }

        let off = q(t[1]);
        let (lo, hi) = (q(t[2]), q(t[3]));
        let shifted = if off == 0.0 { src } else { format!("{src}-{}", fmt_num(off)) };
        writeln!(g.body, "let t{}={shifted};", i + 1).ok()?;
        if hi - lo > 0.0 {
            writeln!(
                g.body,
                "if(t{n}>={h})t{n}={l}+((t{n}-{l})%{p});",
                n = i + 1,
                h = fmt_num(hi),
                l = fmt_num(lo),
                p = fmt_num(hi - lo)
            )
            .ok()?;
        }
    }

    // Visibility gates, evaluated once per frame. A binding inside a layer that
    // is off is then skipped by a single boolean test, which is what keeps a
    // scene of staggered layers from paying for all of it every frame.
    for (i, gate) in scene.data.gates.iter().enumerate() {
        writeln!(
            g.body,
            "const G{i}=f>={}&&f<{};",
            fmt_num(q(gate[0])),
            fmt_num(q(gate[1]))
        )
        .ok()?;
    }

    // Layer records, as objects with handles — the same shape `rec.js`
    // materializes from the stream, emitted directly instead.
    if !scene.data.layers.is_empty() {
        let mut rows = Vec::with_capacity(scene.data.layers.len());
        for r in &scene.data.layers {
            let mut fields = vec![format!("i:{}", r.i)];
            if let Some(n) = r.n {
                let name = scene.data.names.get(n as usize).cloned().unwrap_or_default();
                fields.push(format!("n:{}", js_string(&name)));
            }
            if let Some(pr) = r.pr {
                fields.push(format!("pr:{pr}"));
            }
            for (key, p, default) in [
                ("p", &r.p, RECORD_DEFAULTS[0]),
                ("a", &r.a, RECORD_DEFAULTS[1]),
                ("sc", &r.sc, RECORD_DEFAULTS[2]),
                ("r", &r.r, RECORD_DEFAULTS[3]),
                ("o", &r.o, RECORD_DEFAULTS[4]),
                ("h", &r.h, RECORD_DEFAULTS[5]),
            ] {
                // A property equal to its default is elided, exactly as on the
                // wire — the engine and the binders supply the same defaults.
                let keep = p.as_ref().filter(|p| !default.is_some_and(|d| p.is_exactly(d)));
                match keep {
                    Some(p) => {
                        let h = g.handle(p, &easings);
                        fields.push(format!("{key}:{h}"));
                    }
                    None => fields.push(format!("{key}:null")),
                }
            }
            fields.push(format!("ef:{}", effects_literal(&mut g, &r.ef, &easings)));
            rows.push(format!("{{{}}}", fields.join(",")));
        }
        writeln!(g.init, "ctx.recs=[{}];", rows.join(",")).ok()?;
        g.need("initExpr");
        // An emitted table never went through `records`, so the back-pointers
        // `lyAt`/`lyRel`/`lyParent` index through are stamped here instead.
        writeln!(g.init, "lyLink(ctx.recs);").ok()?;
        g.need("lyLink");
    }

    for (bi, b) in scene.data.b.iter().enumerate() {
        let el = match els.iter().position(|e| *e == b.el_index) {
            Some(i) => i,
            None => {
                els.push(b.el_index);
                els.len() - 1
            }
        };
        let gate = scene.data.bind_gate.get(bi).copied().unwrap_or(0);
        if gate != 0 {
            writeln!(g.body, "if(G{}){{", gate - 1).ok()?;
        }
        // A binding on a precomp clock reads that slot's time. Shadowing `f`
        // is what lets every property emitter stay clock-agnostic.
        let slot_of = scene.data.slots.get(bi).copied().unwrap_or(0);
        if slot_of != 0 {
            writeln!(g.body, "{{const f=t{slot_of};").ok()?;
        }
        emit_binding(&mut g, b, el, bi, &easings, &mut slots)?;
        if slot_of != 0 {
            writeln!(g.body, "}}").ok()?;
        }
        if gate != 0 {
            writeln!(g.body, "}}").ok()?;
        }
    }

    Some(Generated {
        body: g.body,
        init: g.init,
        exprs: !scene.data.layers.is_empty() || g.handles > 0,
        paths: g.paths,
        pre: g.pre,
        decls: g.decls,
        easings: g.easings,
        needs: g.needs,
        slots,
        els,
    })
}

/// The finished pieces of a generated module.
pub struct Generated {
    pub body: String,
    /// Runs once inside `init`, after the expression engine is installed.
    pub init: String,
    /// Whether this module needs the expression engine at all.
    pub exprs: bool,
    pub paths: Vec<String>,
    pub pre: String,
    pub decls: Vec<String>,
    pub easings: Vec<[f64; 4]>,
    pub needs: Vec<&'static str>,
    pub slots: Vec<String>,
    pub els: Vec<u32>,
}

fn emit_binding(
    g: &mut Builder,
    b: &Binding,
    el: usize,
    bi: usize,
    easings: &[[f64; 4]],
    slots: &mut Vec<String>,
) -> Option<()> {
    let prop = |i: usize| -> Option<&Prop> {
        match b.args.get(i) {
            Some(Arg::Prop(p)) => Some(p),
            _ => None,
        }
    };
    let slot = |slots: &mut Vec<String>, tag: &str| {
        let s = format!("s{bi}{tag}");
        slots.push(s.clone());
        s
    };

    match b.op {
        op::TRANSFORM => {
            let p = g.prop(prop(0)?, 2, easings);
            let a = g.prop(prop(1)?, 2, easings);
            let s = g.prop(prop(2)?, 2, easings);
            let r = g.prop(prop(3)?, 1, easings);

            // scale is a percentage; rotation is degrees.
            let sx = mul(&s[0], &Val::Lit(0.01));
            let sy = mul(&s[1], &Val::Lit(0.01));

            // A rotation the planner proved constant collapses the matrix. At
            // zero the linear part is diagonal and the trig disappears entirely.
            let (m0, m1, m2, m3) = match r[0].lit() {
                Some(deg) if deg == 0.0 => (sx.clone(), Val::Lit(0.0), Val::Lit(0.0), sy.clone()),
                Some(deg) => {
                    let rad = deg * std::f64::consts::PI / 180.0;
                    let (cs, sn) = (Val::Lit(rad.cos()), Val::Lit(rad.sin()));
                    (
                        mul(&cs, &sx),
                        mul(&sn, &sx),
                        mul(&Val::Lit(-1.0), &mul(&sn, &sy)),
                        mul(&cs, &sy),
                    )
                }
                None => {
                    let th = g.name();
                    let cs = g.name();
                    let sn = g.name();
                    writeln!(
                        g.body,
                        "const {th}={}*Math.PI/180,{cs}=Math.cos({th}),{sn}=Math.sin({th});",
                        paren(&r[0])
                    )
                    .unwrap();
                    (
                        mul(&Val::Expr(cs.clone()), &sx),
                        mul(&Val::Expr(sn.clone()), &sx),
                        mul(&Val::Lit(-1.0), &mul(&Val::Expr(sn), &sy)),
                        mul(&Val::Expr(cs), &sy),
                    )
                }
            };
            let tx = sub(&p[0], &add(&mul(&m0, &a[0]), &mul(&m2, &a[1])));
            let ty = sub(&p[1], &add(&mul(&m1, &a[0]), &mul(&m3, &a[1])));

            g.need("r5");
            g.need("r2");
            let value = format!(
                "'matrix('+{}+','+{}+','+{}+','+{}+','+{}+','+{}+')'",
                num5(g, &m0),
                num5(g, &m1),
                num5(g, &m2),
                num5(g, &m3),
                num2(g, &tx),
                num2(g, &ty),
            );
            let sl = slot(slots, "t");
            g.write(el, "transform", &value, &sl);
        }
        op::TRANSLATE => {
            // Absent means the identity linear part; the interpreter's binder
            // defaults to the same spelling.
            let prefix = match b.args.first() {
                Some(Arg::Str(s)) => s.clone(),
                Some(Arg::Null) => "translate(".to_string(),
                _ => return None,
            };
            let ex = match b.args.get(1) {
                Some(Arg::Num(v)) => *v,
                _ => return None,
            };
            let ey = match b.args.get(2) {
                Some(Arg::Num(v)) => *v,
                _ => return None,
            };
            let p = g.prop(prop(3)?, 2, easings);
            g.need("r2");
            let value = format!(
                "{}+{}+','+{}+')'",
                js_string(&prefix),
                num2(g, &add(&p[0], &Val::Lit(ex))),
                num2(g, &add(&p[1], &Val::Lit(ey))),
            );
            let sl = slot(slots, "t");
            g.write(el, "transform", &value, &sl);
        }
        op::OPACITY => {
            let o = g.prop(prop(0)?, 1, easings);
            g.need("r");
            let value = num(g, &mul(&o[0], &Val::Lit(0.01)));
            let sl = slot(slots, "o");
            g.write(el, "opacity", &value, &sl);
        }
        op::DISPLAY => {
            let (ip, opp) = match (b.args.first(), b.args.get(1)) {
                (Some(Arg::Num(a)), Some(Arg::Num(b2))) => (*a, *b2),
                _ => return None,
            };
            let sl = slot(slots, "d");
            writeln!(
                g.body,
                "{{const w=f>={}&&f<{};if(w!=={sl}){{{sl}=w;e{el}.style.display=w?'':'none'}}}}",
                fmt_num(q(ip)),
                fmt_num(q(opp))
            )
            .unwrap();
        }
        op::FILL | op::STROKE => {
            let stroke = b.op == op::STROKE;
            let name = if stroke { "stroke" } else { "fill" };
            let o = g.prop(prop(1)?, 1, easings);
            let alpha = mul(&o[0], &Val::Lit(0.01));

            match b.args.first() {
                // A null colour means the paint is a gradient already baked into
                // the markup, and only its opacity varies.
                Some(Arg::Null) => {
                    g.need("r");
                    let value = num(g, &alpha);
                    let sl = slot(slots, "o");
                    g.write(el, &format!("{name}-opacity"), &value, &sl);
                }
                Some(Arg::Prop(p)) => {
                    let c = g.prop(p, 4, easings);
                    g.need("css");
                    let value = format!(
                        "css([{},{},{}],{})",
                        c[0].js(),
                        c[1].js(),
                        c[2].js(),
                        o[0].js()
                    );
                    let sl = slot(slots, "c");
                    g.write(el, name, &value, &sl);
                }
                _ => return None,
            }
            if stroke {
                let w = g.prop(prop(2)?, 1, easings);
                g.need("r");
                let value = num(g, &w[0]);
                let sl = slot(slots, "w");
                g.write(el, "stroke-width", &value, &sl);
            }
        }
        op::SHAPE => {
            let list = match b.args.first() {
                Some(Arg::List(items)) => items,
                _ => return None,
            };
            let kind = match list.first() {
                Some(Arg::Tag(t)) => *t,
                Some(Arg::Num(v)) => *v as u32,
                _ => return None,
            };
            let arg = |i: usize| -> Option<&Prop> {
                match list.get(i) {
                    Some(Arg::Prop(p)) => Some(p),
                    _ => None,
                }
            };

            // The geometry generators write into one scratch object per
            // binding, so a steady-state frame allocates nothing.
            let src = match kind {
                0 => g.path_prop(arg(1)?, easings)?,
                1 => {
                    g.need("rectPath");
                    let scratch = g.scratch(bi);
                    let sz = g.prop(arg(1)?, 2, easings);
                    let ps = g.prop(arg(2)?, 2, easings);
                    let rd = g.prop(arg(3)?, 1, easings);
                    format!(
                        "rectPath({scratch},{},{},{},{},{})",
                        ps[0].js(), ps[1].js(), sz[0].js(), sz[1].js(), rd[0].js()
                    )
                }
                2 => {
                    g.need("ellipsePath");
                    let scratch = g.scratch(bi);
                    let sz = g.prop(arg(1)?, 2, easings);
                    let ps = g.prop(arg(2)?, 2, easings);
                    format!(
                        "ellipsePath({scratch},{},{},{},{})",
                        ps[0].js(),
                        ps[1].js(),
                        mul(&sz[0], &Val::Lit(0.5)).js(),
                        mul(&sz[1], &Val::Lit(0.5)).js()
                    )
                }
                3 => {
                    g.need("starPath");
                    let scratch = g.scratch(bi);
                    let sy = match list.get(1) {
                        Some(Arg::Tag(t)) => *t as f64,
                        Some(Arg::Num(v)) => *v,
                        _ => return None,
                    };
                    let pt = g.prop(arg(2)?, 1, easings);
                    let ps = g.prop(arg(3)?, 2, easings);
                    let or = g.prop(arg(4)?, 1, easings);
                    let ir = g.prop(arg(5)?, 1, easings);
                    let rt = g.prop(arg(6)?, 1, easings);
                    format!(
                        "starPath({scratch},{},{},{},{},{},{},{})",
                        fmt_num(sy), pt[0].js(), ps[0].js(), ps[1].js(),
                        or[0].js(), ir[0].js(), rt[0].js()
                    )
                }
                _ => return None,
            };

            g.need("pathD");
            let sl = slot(slots, "d");
            match b.args.get(1) {
                Some(Arg::List(tm)) if tm.len() >= 3 => {
                    let getp = |i: usize| match tm.get(i) {
                        Some(Arg::Prop(p)) => Some(p),
                        _ => None,
                    };
                    let ts = g.prop(getp(0)?, 1, easings);
                    let te = g.prop(getp(1)?, 1, easings);
                    let to = g.prop(getp(2)?, 1, easings);
                    g.need("trimTable");
                    g.need("trimApply");
                    // A static source path has one arc-length table for the
                    // whole animation; a moving one has to be re-measured.
                    let fixed = matches!(list.get(1), Some(Arg::Prop(Prop::Path(_))));
                    let tab = if fixed {
                        let t = format!("T{}", g.tabs);
                        g.tabs += 1;
                        writeln!(g.pre, "const {t}=trimTable({src});").ok()?;
                        t
                    } else {
                        String::new()
                    };
                    let hide = slot(slots, "h");
                    let name = g.name();
                    writeln!(g.body, "const {name}={src};").ok()?;
                    writeln!(
                        g.body,
                        "{{const a={}/100,b={}/100,lo=a<b?a:b,hi=a<b?b:a,vis=hi-lo;\
let out=null,hide=false;\
if(vis<=0)hide=true;else if(vis<1){{out=trimApply({},lo,hi,{}/360);if(out&&!out.v.length)hide=true}}\
if(hide!=={hide}){{{hide}=hide;e{el}.style.display=hide?'none':''}}\
if(!hide){{const w=pathD(out||{name});if(w!=={sl}){{{sl}=w;e{el}.setAttribute('d',w)}}}}}}",
                        ts[0].js(),
                        te[0].js(),
                        if fixed { tab } else { format!("trimTable({name})") },
                        to[0].js()
                    )
                    .ok()?;
                }
                _ => {
                    let value = format!("pathD({src})");
                    g.write(el, "d", &value, &sl);
                }
            }
        }
        op::RECT => {
            g.need("r");
            let sz = g.prop(prop(0)?, 2, easings);
            let ps = g.prop(prop(1)?, 2, easings);
            let rd = g.prop(prop(2)?, 1, easings);
            let half = |v: &Val| mul(v, &Val::Lit(0.5));
            let x = sub(&ps[0], &half(&sz[0]));
            let y = sub(&ps[1], &half(&sz[1]));
            for (attr, v, tag) in [
                ("x", x, "x"), ("y", y, "y"),
                ("width", sz[0].clone(), "w"), ("height", sz[1].clone(), "h"),
            ] {
                let value = num(g, &v);
                let sl = slot(slots, tag);
                g.write(el, attr, &value, &sl);
            }
            // The corner radius clamps to half the smaller side, so it is only
            // constant-foldable when every input is.
            if let (Some(rr), Some(w), Some(h)) = (rd[0].lit(), sz[0].lit(), sz[1].lit()) {
                if rr > 0.0 {
                    let c = rr.min(w / 2.0).min(h / 2.0);
                    for (attr, tag) in [("rx", "rx"), ("ry", "ry")] {
                        let value = num(g, &Val::Lit(c));
                        let sl = slot(slots, tag);
                        g.write(el, attr, &value, &sl);
                    }
                }
            } else {
                let name = g.name();
                writeln!(
                    g.body,
                    "const {name}=Math.min({},{},{});",
                    rd[0].js(),
                    half(&sz[0]).js(),
                    half(&sz[1]).js()
                )
                .ok()?;
                for (attr, tag) in [("rx", "rx"), ("ry", "ry")] {
                    let value = num(g, &Val::Expr(name.clone()));
                    let sl = slot(slots, tag);
                    g.write(el, attr, &value, &sl);
                }
            }
        }
        op::ELLIPSE => {
            g.need("r");
            let sz = g.prop(prop(0)?, 2, easings);
            let ps = g.prop(prop(1)?, 2, easings);
            for (attr, v, tag) in [
                ("cx", ps[0].clone(), "cx"),
                ("cy", ps[1].clone(), "cy"),
                ("rx", mul(&sz[0], &Val::Lit(0.5)), "rx"),
                ("ry", mul(&sz[1], &Val::Lit(0.5)), "ry"),
            ] {
                let value = num(g, &v);
                let sl = slot(slots, tag);
                g.write(el, attr, &value, &sl);
            }
        }
        op::GRADIENT => {
            let radial = matches!(b.args.first(), Some(Arg::Tag(2)));
            let sp = g.prop(prop(1)?, 2, easings);
            let ep = g.prop(prop(2)?, 2, easings);
            g.need("r");
            let mut writes: Vec<(&str, Val, &str)> = vec![
                (if radial { "cx" } else { "x1" }, sp[0].clone(), "a"),
                (if radial { "cy" } else { "y1" }, sp[1].clone(), "b"),
            ];
            if radial {
                let dx = sub(&ep[0], &sp[0]);
                let dy = sub(&ep[1], &sp[1]);
                writes.push(("r", Val::Expr(format!("Math.hypot({},{})", dx.js(), dy.js())), "c"));
            } else {
                writes.push(("x2", ep[0].clone(), "c"));
                writes.push(("y2", ep[1].clone(), "d"));
            }
            for (attr, v, tag) in writes {
                let value = num(g, &v);
                let sl = slot(slots, tag);
                g.write(el, attr, &value, &sl);
            }
        }
        op::LAYER_TX | op::LAYER_OP => {
            // These name a record rather than carrying a second copy of the
            // same keyframes, and every input is a runtime handle — so there is
            // nothing to fold and nothing to gain from inlining. One call.
            let ri = match b.args.first() {
                Some(Arg::Num(n)) => *n as usize,
                _ => return None,
            };
            let helper = if b.op == op::LAYER_TX { "layerTx" } else { "layerOp" };
            g.need(if b.op == op::LAYER_TX { "layerTx" } else { "layerOp" });
            let name = format!("u{bi}");
            writeln!(g.init, "const {name}={helper}(e{el},ctx.recs[{ri}]);").ok()?;
            writeln!(g.body, "{name}(f);").ok()?;
        }
        _ => return None,
    }
    Some(())
}

/// Format a value for output, folding when the compiler already knows it.
fn num(g: &mut Builder, v: &Val) -> String {
    match v.lit() {
        Some(x) => js_string(&fmt_num(x)),
        None => {
            g.need("r");
            format!("r({})", v.js())
        }
    }
}

fn num2(g: &mut Builder, v: &Val) -> String {
    match v.lit() {
        Some(x) => js_string(&crate::scene::svg::nd(x, 100.0)),
        None => {
            g.need("r2");
            format!("r2({})", v.js())
        }
    }
}

fn num5(g: &mut Builder, v: &Val) -> String {
    match v.lit() {
        Some(x) => js_string(&crate::scene::svg::nd(x, 100000.0)),
        None => {
            g.need("r5");
            format!("r5({})", v.js())
        }
    }
}

/// Effects as a literal, with each parameter's property emitted as a handle.
fn effects_literal(g: &mut Builder, list: &[Effect], easings: &[[f64; 4]]) -> String {
    if list.is_empty() {
        return "null".into();
    }
    let opt = |v: &Option<String>| match v {
        Some(s) => js_string(s),
        None => "null".into(),
    };
    let entries: Vec<String> = list
        .iter()
        .map(|e| {
            let params: Vec<String> = e
                .ef
                .iter()
                .map(|p| {
                    let handle = match &p.p {
                        Some(prop) => g.handle(prop, easings),
                        None => "null".into(),
                    };
                    let v = match p.v {
                        Some(v) if p.ty == 10 => fmt_num(v),
                        Some(v) => fmt_num(q(v)),
                        None => "undefined".into(),
                    };
                    format!(
                        "{{nm:{},mn:{},ty:{},v:{v},p:{handle}}}",
                        opt(&p.nm),
                        opt(&p.mn),
                        p.ty
                    )
                })
                .collect();
            format!("{{nm:{},mn:{},ef:[{}]}}", opt(&e.nm), opt(&e.mn), params.join(","))
        })
        .collect();
    format!("[{}]", entries.join(","))
}

fn js_string(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}
