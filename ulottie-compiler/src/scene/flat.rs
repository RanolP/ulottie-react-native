//! Flat integer wire format.
//!
//! Everything time-varying is written into **one `Vec<i32>`** and shipped as a
//! single VLQ base36 string. There are no nested arrays, no polymorphic cells
//! and no objects: a property is a run of integers at an offset, and every
//! reference to it is that offset. The runtime decodes the string into one
//! `Int32Array` and reads properties straight out of it, so mount allocates a
//! closure per property and nothing else.
//!
//! Three things make this lossless and compact at the same time:
//!
//! * **Values are already quantized.** [`svg::q`] rounds every number that
//!   reaches the wire to 3 decimals, so scaling by a power of ten is exact —
//!   this is a change of representation, not of precision.
//! * **The scale is per column, not global.** Keyframe times are usually whole
//!   frames and values often whole units, so each column picks the smallest
//!   power of ten that represents it exactly. A time column of `[0,25,40]`
//!   stays three characters instead of becoming `[0,25000,40000]`.
//! * **Properties are hash-consed.** Two properties with identical encodings
//!   collapse to one offset. Instancing already deduplicates whole precomps;
//!   this catches the rest, and it is why `ripple`'s layer table stops paying
//!   per copy for props that were bit-identical all along.

use std::collections::HashMap;

use super::prop::{Anim, AnimKind, Prop};
use super::svg::{q, FlatPath};

/// Tag occupies the low 3 bits of a property's first word; the shifts and
/// flags ride above it.
pub mod tag {
    pub const SCALAR: i32 = 0;
    pub const VECTOR: i32 = 1;
    pub const PATH: i32 = 2;
    pub const ANIM: i32 = 3;
    pub const EXPR: i32 = 4;
}

/// `Anim` header flag bits, above the tag and the two shifts.
pub mod anim {
    /// Explicit segment end values (legacy Lottie `e`).
    pub const END: i32 = 1;
    /// Per-segment easing indices.
    pub const EASE: i32 = 2;
    /// Per-segment hold flags.
    pub const HOLD: i32 = 4;
    /// Spatial tangents (`to`/`ti`).
    pub const SPATIAL: i32 = 8;
}

/// Largest power of ten a column is allowed to scale by. Matches [`svg::q`]:
/// beyond this there is nothing left to represent.
const MAX_SHIFT: u32 = 3;

const POW10: [f64; 4] = [1.0, 10.0, 100.0, 1000.0];

/// Smallest shift in `0..=MAX_SHIFT` that represents every value exactly.
///
/// Every input has already been through [`q`], so `MAX_SHIFT` always succeeds
/// and the loop is looking for something cheaper.
fn shift_for(vals: impl IntoIterator<Item = f64>) -> u32 {
    let mut s = 0u32;
    for x in vals {
        let qx = q(x);
        while s < MAX_SHIFT {
            let m = POW10[s as usize];
            if (qx * m).round() / m == qx {
                break;
            }
            s += 1;
        }
        if s == MAX_SHIFT {
            break;
        }
    }
    s
}

/// A value that does not survive the trip through `i32`.
///
/// Only reachable for coordinates in the millions of user units, which do not
/// describe anything renderable. Failing loudly beats shipping a silently
/// truncated animation.
#[derive(Debug)]
pub struct Overflow(pub f64);

impl std::fmt::Display for Overflow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "value {} is too large for the integer wire format (limit ±{})",
            self.0,
            i32::MAX / 1000
        )
    }
}

impl std::error::Error for Overflow {}

/// Fixed slots at the head of the stream. Offset 0 doubles as the "absent"
/// marker, which works because the header sits there and no property can.
pub mod head {
    pub const FR: usize = 1;
    pub const IP: usize = 2;
    pub const OP: usize = 3;
    /// bit 0 = markup has ids to unique per mount, bit 1 = per clone.
    pub const FLAGS: usize = 4;
    pub const EASINGS: usize = 5;
    pub const TIMELINES: usize = 6;
    pub const GATES: usize = 7;
    pub const SLOTS: usize = 8;
    pub const BIND_GATE: usize = 9;
    pub const SCOPES: usize = 10;
    pub const BINDINGS: usize = 11;
    pub const LAYERS: usize = 12;
    pub const ASSETS: usize = 13;
    pub const USES: usize = 14;
    pub const REMAPS: usize = 15;
    pub const TEMPLATES: usize = 16;
    pub const LEN: usize = 17;
}

/// Presence bits for a layer record, in its first word.
pub mod rec {
    pub const NAME: i32 = 1;
    pub const PARENT: i32 = 1 << 1;
    pub const P: i32 = 1 << 2;
    pub const A: i32 = 1 << 3;
    pub const SC: i32 = 1 << 4;
    pub const R: i32 = 1 << 5;
    pub const O: i32 = 1 << 6;
    pub const H: i32 = 1 << 7;
    pub const EFFECTS: i32 = 1 << 8;
}

pub struct Flat {
    ints: Vec<i32>,
    /// Encoded property → offset, so identical properties are written once.
    pool: HashMap<Vec<i32>, u32>,
    /// The one place text survives: layer names, baked matrix prefixes,
    /// template markup and effect names. Interned, because effect names repeat
    /// across every layer that carries the same control.
    strings: Vec<String>,
    strpool: HashMap<String, u32>,
    /// First value that overflowed, reported once at the end.
    bad: Option<f64>,
}

impl Default for Flat {
    fn default() -> Self {
        Self::new()
    }
}

impl Flat {
    pub fn new() -> Self {
        // The header occupies the low slots, which also makes offset 0 an
        // unambiguous "absent" marker: no property can ever live there.
        Self {
            ints: vec![0; head::LEN],
            pool: HashMap::new(),
            strings: Vec::new(),
            strpool: HashMap::new(),
            bad: None,
        }
    }

    pub fn as_slice(&self) -> &[i32] {
        &self.ints
    }

    pub fn strings(&self) -> &[String] {
        &self.strings
    }

    /// Intern a string, returning its index in the pool.
    pub fn intern(&mut self, s: &str) -> u32 {
        if let Some(i) = self.strpool.get(s) {
            return *i;
        }
        let i = self.strings.len() as u32;
        self.strings.push(s.to_string());
        self.strpool.insert(s.to_string(), i);
        i
    }

    /// Intern, biased by one so that 0 can mean "absent".
    fn intern1(&mut self, s: Option<&String>) -> i32 {
        s.map_or(0, |s| self.intern(s) as i32 + 1)
    }

    /// Append a section and return its offset. An empty section returns 0, so
    /// the header slot reads as absent.
    pub fn section(&mut self, vals: &[i32]) -> u32 {
        if vals.is_empty() {
            return 0;
        }
        let at = self.ints.len() as u32;
        self.ints.extend_from_slice(vals);
        at
    }

    /// `[count, …rows]`, or 0 when there are no rows.
    fn counted(&mut self, count: usize, rows: &[i32]) -> u32 {
        if count == 0 {
            return 0;
        }
        let at = self.ints.len() as u32;
        self.ints.push(count as i32);
        self.ints.extend_from_slice(rows);
        at
    }

    fn set_head(&mut self, slot: usize, v: u32) {
        self.ints[slot] = v as i32;
    }

    fn scaled(&mut self, x: f64, shift: u32) -> i32 {
        let v = (q(x) * POW10[shift as usize]).round();
        if v.is_finite() && v >= i32::MIN as f64 && v <= i32::MAX as f64 {
            v as i32
        } else {
            self.bad.get_or_insert(x);
            0
        }
    }

    /// The overflowing value, if the encoder hit one.
    pub fn overflow(&self) -> Option<Overflow> {
        self.bad.map(Overflow)
    }

    /// Write a property and return its offset, reusing an identical one.
    pub fn prop(&mut self, p: &Prop) -> u32 {
        let body = self.encode_prop(p);
        if let Some(off) = self.pool.get(&body) {
            return *off;
        }
        let at = self.ints.len() as u32;
        self.ints.extend_from_slice(&body);
        self.pool.insert(body, at);
        at
    }

    /// `0` for an absent property — the guard slot makes that unambiguous.
    pub fn opt_prop(&mut self, p: Option<&Prop>) -> u32 {
        p.map_or(0, |p| self.prop(p))
    }

    fn encode_prop(&mut self, p: &Prop) -> Vec<i32> {
        match p {
            Prop::Scalar(v) => {
                let s = shift_for([*v]);
                vec![tag::SCALAR | (s as i32) << 3, self.scaled(*v, s)]
            }
            Prop::Vector(v) => {
                let s = shift_for(v.iter().copied());
                let mut out = vec![tag::VECTOR | (s as i32) << 3, v.len() as i32];
                out.extend(v.iter().map(|x| self.scaled(*x, s)));
                out
            }
            Prop::Path(fp) => self.encode_path(fp),
            Prop::Anim(a) => self.encode_anim(a),
            Prop::Expr { id, fallback, layer } => {
                // The fallback is a property in its own right, so it is pooled
                // like any other and referenced by offset.
                let fb = fallback.as_deref().map_or(0, |f| self.prop(f));
                vec![
                    tag::EXPR,
                    *id as i32,
                    fb as i32,
                    // +1 so that 0 can mean "no layer" — layer 0 is real.
                    layer.map_or(0, |l| l as i32 + 1),
                ]
            }
        }
    }

    /// `[tag|shift<<3|closed<<5|tangents<<6, pointCount, x,y…, (i…), (o…)]`
    ///
    /// Tangents are dropped when every one is zero, which is the common case:
    /// rectangles, polystars and traced outlines are all polygonal.
    fn encode_path(&mut self, fp: &FlatPath) -> Vec<i32> {
        let curved = fp.i.iter().chain(fp.o.iter()).any(|x| *x != 0.0);
        let s = shift_for(fp.v.iter().chain(&fp.i).chain(&fp.o).copied());
        let head = tag::PATH
            | (s as i32) << 3
            | (fp.c as i32) << 5
            | (curved as i32) << 6;
        let mut out = vec![head, (fp.v.len() / 2) as i32];
        out.extend(fp.v.iter().map(|x| self.scaled(*x, s)));
        if curved {
            out.extend(fp.i.iter().map(|x| self.scaled(*x, s)));
            out.extend(fp.o.iter().map(|x| self.scaled(*x, s)));
        }
        out
    }

    /// ```text
    /// [tag | tShift<<3 | vShift<<5 | flags<<7 | kind<<11 | (dim-1)<<13, count,
    ///  t…,                       count
    ///  v…,                       count*dim, or count path offsets
    ///  e…,   if END              same shape as v
    ///  ez…,  if EASE             count-1
    ///  h…,   if HOLD             count-1
    ///  to…, ti…, if SPATIAL      count*dim each ]
    /// ```
    ///
    /// Times and values carry separate shifts because they behave differently:
    /// times are whole frames far more often than values are whole units.
    fn encode_anim(&mut self, a: &Anim) -> Vec<i32> {
        let n = a.t.len();
        let path_kind = a.kind == AnimKind::Path;

        // Path values live as pooled path properties, referenced by offset, so
        // a shape that recurs across keyframes is stored once.
        let v_offs: Vec<i32> = if path_kind {
            a.paths.iter().map(|p| self.prop(&Prop::Path(p.clone())) as i32).collect()
        } else {
            Vec::new()
        };
        let e_offs: Vec<i32> = match &a.end_paths {
            Some(ps) => ps.iter().map(|p| self.prop(&Prop::Path(p.clone())) as i32).collect(),
            None => Vec::new(),
        };

        let ts = shift_for(a.t.iter().copied());
        let vs = if path_kind {
            0
        } else {
            shift_for(
                a.v.iter()
                    .chain(a.end.iter().flatten())
                    .chain(a.to.iter().flatten())
                    .chain(a.ti.iter().flatten())
                    .copied(),
            )
        };

        let mut flags = 0;
        if a.end.is_some() || a.end_paths.is_some() {
            flags |= anim::END;
        }
        if a.ez.is_some() {
            flags |= anim::EASE;
        }
        if a.hold.is_some() {
            flags |= anim::HOLD;
        }
        if a.to.is_some() {
            flags |= anim::SPATIAL;
        }

        // `kind` and `dim` are two bits each and the header word had twenty-one
        // to spare, so they ride in it rather than costing an integer apiece on
        // every keyframed property. `dim` is stored less one: it is never zero.
        let head = tag::ANIM
            | (ts as i32) << 3
            | (vs as i32) << 5
            | flags << 7
            | (a.kind as i32) << 11
            | (a.dim as i32 - 1) << 13;
        let mut out = vec![head, n as i32];
        out.extend(a.t.iter().map(|x| self.scaled(*x, ts)));
        if path_kind {
            out.extend_from_slice(&v_offs);
        } else {
            out.extend(a.v.iter().map(|x| self.scaled(*x, vs)));
        }
        if let Some(e) = &a.end {
            out.extend(e.iter().map(|x| self.scaled(*x, vs)));
        } else if !e_offs.is_empty() {
            out.extend_from_slice(&e_offs);
        }
        if let Some(z) = &a.ez {
            out.extend(z.iter().map(|x| *x as i32));
        }
        if let Some(h) = &a.hold {
            out.extend(h.iter().map(|x| *x as i32));
        }
        if let Some(to) = &a.to {
            // Both tangent columns are per *segment*, so `(n-1)*dim` — one
            // keyframe shorter than the value column. The reader derives `ti`'s
            // start from that, so a mismatch here silently shifts every
            // in-tangent; pin it rather than trust the planner to agree.
            let ti = a.ti.as_deref().unwrap_or(&[]);
            debug_assert_eq!(to.len(), (n - 1) * a.dim, "spatial `to` is per segment");
            debug_assert_eq!(ti.len(), to.len(), "spatial tangents come in pairs");
            out.extend(to.iter().map(|x| self.scaled(*x, vs)));
            out.extend(ti.iter().map(|x| self.scaled(*x, vs)));
        }
        out
    }

    /// The stream as one VLQ base36 string.
    pub fn encode(&self) -> String {
        encode_ints(&self.ints)
    }
}

// ---------------------------------------------------------------------------
// Flattening a planned scene
// ---------------------------------------------------------------------------

/// Defaults every read site supplies for a layer record, so a property equal to
/// one of them needs no wire entry at all.
///
/// These must stay in step with ops/layer.js and expr.js. `o` defaults to
/// **100**, not 0 — eliding an explicit `o: 0` would turn a hidden layer fully
/// opaque.
pub const RECORD_DEFAULTS: [Option<&[f64]>; 6] = [
    Some(&[0.0, 0.0, 0.0]),       // p
    Some(&[0.0, 0.0, 0.0]),       // a
    Some(&[100.0, 100.0, 100.0]), // sc
    Some(&[0.0]),                 // r
    Some(&[100.0]),               // o
    None,                         // h — no default; absent means absent
];

/// Move the entire scene into one integer stream.
///
/// Runs after planning, so the planner keeps working in terms of `Prop`,
/// `Binding` and `LayerRecord`, and only the wire sees integers. What comes
/// back is the stream plus the string pool; the payload object is those two
/// and nothing else.
///
/// Order matters only in that properties are written before the structures
/// that reference them — offsets have to exist before they are stored.
/// `scopes` says whether the composition-scope column has a reader. Only the
/// fallback layer lookup keys on it; once every reference in every body has been
/// resolved to a slot there is nothing left to look up, and the column is a few
/// hundred bytes of nothing on an animation like `ripple`.
pub fn flatten(data: &super::SceneData) -> anyhow::Result<Flat> {
    let mut f = Flat::new();

    f.ints[head::FR] = f.scaled(data.fr, 3);
    f.ints[head::IP] = f.scaled(data.ip, 3);
    f.ints[head::OP] = f.scaled(data.op, 3);
    f.ints[head::FLAGS] = data.uses_ids as i32 | (data.uses_clone_ids as i32) << 1;

    // Small fixed-width columns first: they are read once at mount and never
    // referenced by offset, so they can go anywhere.
    let rows: Vec<i32> = data
        .easings
        .iter()
        .flat_map(|e| e.iter().map(|v| f.scaled(*v, 3)).collect::<Vec<_>>())
        .collect();
    let off = f.counted(data.easings.len(), &rows);
    f.set_head(head::EASINGS, off);

    let off = f.timelines_section(&data.timelines);
    f.set_head(head::TIMELINES, off);

    let off = f.gates_section(&data.gates);
    f.set_head(head::GATES, off);

    if data.slots.iter().any(|s| *s != 0) {
        let off = f.deltas(&data.slots);
        f.set_head(head::SLOTS, off);
    }
    if data.bind_gate.iter().any(|g| *g != 0) {
        let col: Vec<i32> = data.bind_gate.iter().map(|g| *g as i32).collect();
        let off = f.counted(col.len(), &col);
        f.set_head(head::BIND_GATE, off);
    }

    // Layer names are interned before any record is written: a record stores
    // a pool index, and the planner's own name table is a different numbering.
    // Storing the planner's index made `thisComp.layer('name')` look up the
    // wrong string, so every expression that reached for another layer threw
    // and silently fell back to its static value — a frozen animation with no
    // error in the console.
    let names: Vec<u32> = data.names.iter().map(|n| f.intern(n)).collect();


    let off = f.bindings_section(&data.b);
    f.set_head(head::BINDINGS, off);

    let off = f.records_section(&data.layers, &names);
    f.set_head(head::LAYERS, off);

    // Assets are planned once and replayed, so each one carries its own
    // bindings, slots, timelines and records as independent sections.
    let mut assets = Vec::with_capacity(data.assets.len() * 5);
    for a in &data.assets {
        let b = f.bindings_section(&a.bindings);
        let s = if a.slots.iter().any(|x| *x != 0) { f.deltas(&a.slots) } else { 0 };
        let t = f.timelines_section(&a.timelines);
        let y = f.records_section(&a.records, &names);
        assets.extend([a.template as i32, b as i32, s as i32, t as i32, y as i32]);
    }
    let off = f.counted(data.assets.len(), &assets);
    f.set_head(head::ASSETS, off);

    let uses: Vec<i32> = data
        .uses
        .iter()
        .flat_map(|u| {
            [u.asset, u.el_base, u.rec_base, u.slot_base, u.parent_slot, u.scope].map(|v| v as i32)
        })
        .collect();
    let off = f.counted(data.uses.len(), &uses);
    f.set_head(head::USES, off);

    if data.remaps.iter().any(|r| r.is_some()) {
        let col: Vec<i32> = data
            .remaps
            .iter()
            .map(|p| f.opt_prop(p.as_ref()) as i32)
            .collect();
        let off = f.counted(col.len(), &col);
        f.set_head(head::REMAPS, off);
    }

    match f.overflow() {
        Some(e) => Err(anyhow::anyhow!(e)),
        None => Ok(f),
    }
}

impl Flat {
    /// An ascending column as first differences, which is what makes an
    /// instanced asset's indices replayable at any base.
    fn deltas(&mut self, col: &[u32]) -> u32 {
        let mut prev = 0i64;
        let rows: Vec<i32> = col
            .iter()
            .map(|v| {
                let d = *v as i64 - prev;
                prev = *v as i64;
                d as i32
            })
            .collect();
        self.counted(rows.len(), &rows)
    }

    /// `[count, shift, (ip, op) × count]` — layer visibility windows, which
    /// are whole frames even more often than clocks are.
    fn gates_section(&mut self, rows: &[[f64; 2]]) -> u32 {
        if rows.is_empty() {
            return 0;
        }
        let shift = shift_for(rows.iter().flat_map(|g| [g[0], g[1]]));
        let mut flat = Vec::with_capacity(rows.len() * 2 + 1);
        flat.push(shift as i32);
        for g in rows {
            for k in 0..2 {
                let v = self.scaled(g[k], shift);
                flat.push(v);
            }
        }
        let at = self.ints.len() as u32;
        self.ints.push(rows.len() as i32);
        self.ints.extend_from_slice(&flat);
        at
    }

    /// `[count, shift, (parentSlot, offset, ip, op) × count]`
    ///
    /// The three time fields share one shift, chosen the same way a property's
    /// columns choose theirs. They are whole frames far more often than not, and
    /// at a fixed ×1000 this table was 16% of the whole stream — `361` written
    /// as `361000` costs five characters instead of two, 230 times over on
    /// `ripple`. The parent slot is an index, not a measurement, so it is never
    /// scaled.
    fn timelines_section(&mut self, rows: &[[f64; 4]]) -> u32 {
        if rows.is_empty() {
            return 0;
        }
        let shift = shift_for(rows.iter().flat_map(|t| [t[1], t[2], t[3]]));
        let mut flat = Vec::with_capacity(rows.len() * 4 + 1);
        flat.push(shift as i32);
        for t in rows {
            flat.push(t[0] as i32);
            for k in 1..4 {
                let v = self.scaled(t[k], shift);
                flat.push(v);
            }
        }
        let at = self.ints.len() as u32;
        self.ints.push(rows.len() as i32);
        self.ints.extend_from_slice(&flat);
        at
    }

    /// `[count, (len, op, elDelta, …args) × count]`.
    ///
    /// The element index and the layer-record index ride as first differences
    /// for the same reason they did on the old wire: consecutive rows differ by
    /// a small number where the absolute values are large and all distinct.
    fn bindings_section(&mut self, list: &[super::Binding]) -> u32 {
        let mut rows = Vec::new();
        let (mut el, mut rec) = (0i64, 0i64);
        for b in list {
            let e = b.el_index as i64;
            let mut args = Vec::with_capacity(b.args.len());
            for (i, a) in b.args.iter().enumerate() {
                // `LAYER_TX`/`LAYER_OP` take a record index first; everything
                // else reads its arguments as values.
                if i == 0 && super::arg0_is_record(b.op) {
                    if let super::Arg::Num(n) = a {
                        let v = *n as i64;
                        args.push((v - rec) as i32);
                        rec = v;
                        continue;
                    }
                }
                args.push(self.arg(a));
            }
            rows.push(args.len() as i32);
            rows.push(b.op as i32);
            rows.push((e - el) as i32);
            rows.extend(args);
            el = e;
        }
        self.counted(list.len(), &rows)
    }

    /// One binding argument as a single integer.
    ///
    /// Which of these an op expects is fixed per op code and known to its
    /// binder, so nothing has to be tagged on the wire.
    fn arg(&mut self, a: &super::Arg) -> i32 {
        match a {
            // A measurement: an in/out point, a baked coordinate. The ×1000
            // keeps a fractional value exact. Enumerations use `Tag`.
            super::Arg::Num(n) => self.scaled(*n, 3),
            super::Arg::Tag(t) => *t as i32,
            // Biased by one, so `Arg::Null` in the same slot reads as absent.
            super::Arg::Str(s) => self.intern1(Some(s)),
            // A nested list becomes its own little section, referenced by
            // offset — the geometry descriptor and the trim triple.
            super::Arg::List(items) => {
                let vals: Vec<i32> = items.iter().map(|i| self.arg_deep(i)).collect();
                self.section(&vals) as i32
            }
            super::Arg::Prop(p) => self.prop(p) as i32,
            // `null` and "absent" are the same thing to every reader.
            super::Arg::Null => 0,
        }
    }

    /// Inside a list every number is a tag or an index rather than a
    /// measurement — the geometry kind, the polystar type, the trim mode — so
    /// the whole list is stored as-is.
    fn arg_deep(&mut self, a: &super::Arg) -> i32 {
        match a {
            super::Arg::Num(n) => *n as i32,
            other => self.arg(other),
        }
    }

    /// `[count, …offsets, (mask, compIndex, …present fields) × count]`.
    ///
    /// Fields are elided when the runtime would default to the same value, so
    /// the mask is what says which of the nine slots are actually there — which
    /// makes rows variable-length, hence the offset table. Expressions address
    /// records by index constantly (`thisComp.layer(n)`, parent chains), and
    /// walking to find one would be quadratic.
    fn records_section(&mut self, list: &[super::LayerRecord], names: &[u32]) -> u32 {
        // Effects are variable-length, so they are written first and the record
        // keeps an offset. Doing it in one pass would interleave them with the
        // record rows and break the fixed stride.
        let effects: Vec<u32> = list.iter().map(|r| self.effects_section(&r.ef)).collect();

        let mut rows = Vec::new();
        let mut index = Vec::with_capacity(list.len());
        for (r, ef) in list.iter().zip(&effects) {
            index.push(rows.len());
            let props = [&r.p, &r.a, &r.sc, &r.r, &r.o, &r.h];
            let offs: Vec<u32> = props
                .iter()
                .zip(RECORD_DEFAULTS)
                .map(|(p, default)| match p {
                    Some(p) if !default.is_some_and(|d| p.is_exactly(d)) => self.prop(p),
                    _ => 0,
                })
                .collect();

            let mut mask = 0;
            if r.n.is_some() {
                mask |= rec::NAME;
            }
            if r.pr.is_some() {
                mask |= rec::PARENT;
            }
            for (i, o) in offs.iter().enumerate() {
                if *o != 0 {
                    mask |= rec::P << i;
                }
            }
            if *ef != 0 {
                mask |= rec::EFFECTS;
            }

            rows.push(mask);
            rows.push(r.i as i32);
            if let Some(n) = r.n {
                rows.push(names[n as usize] as i32);
            }
            if let Some(pr) = r.pr {
                rows.push(pr as i32);
            }
            rows.extend(offs.iter().filter(|o| **o != 0).map(|o| *o as i32));
            if *ef != 0 {
                rows.push(*ef as i32);
            }
        }
        if list.is_empty() {
            return 0;
        }
        // The table has to be written before the rows it points at: count,
        // table, rows. It ships as first differences — it is the one column in
        // the format that ascends and never repeats a value, which is exactly
        // the shape deltas suit. Measured on the corpus: 1018 characters and
        // 526 gzipped bytes down to 270 and 62. Every *other* index column here
        // was left absolute, because they repeat, and repetition is worth more
        // to the compressor than small magnitudes are.
        let at = self.ints.len() as u32;
        let base = at + 1 + list.len() as u32;
        self.ints.push(list.len() as i32);
        let mut prev = 0i64;
        for i in &index {
            let abs = base as i64 + *i as i64;
            self.ints.push((abs - prev) as i32);
            prev = abs;
        }
        self.ints.extend_from_slice(&rows);
        at
    }

    /// `[count, (name, mn, paramCount, (name, mn, ty, value, prop) × n) × count]`
    ///
    /// Names are pool indices biased by one, because an effect without a name
    /// is legal and 0 has to mean absent.
    fn effects_section(&mut self, list: &[super::Effect]) -> u32 {
        let mut rows = Vec::new();
        for e in list {
            let nm = self.intern1(e.nm.as_ref());
            let mn = self.intern1(e.mn.as_ref());
            rows.push(nm);
            rows.push(mn);
            rows.push(e.ef.len() as i32);
            for p in &e.ef {
                let pnm = self.intern1(p.nm.as_ref());
                let pmn = self.intern1(p.mn.as_ref());
                let off = self.opt_prop(p.p.as_ref()) as i32;
                // A layer-control parameter holds a layer *index*, so it must
                // not be scaled; every other literal is a measurement.
                let v = match p.v {
                    Some(v) if p.ty == 10 => (v as i32) << 1 | 1,
                    Some(v) => self.scaled(v, 3) << 1 | 1,
                    None => 0,
                };
                rows.push(pnm);
                rows.push(pmn);
                rows.push(p.ty as i32);
                rows.push(v);
                rows.push(off);
            }
        }
        self.counted(list.len(), &rows)
    }
}

// ---------------------------------------------------------------------------
// VLQ base36
// ---------------------------------------------------------------------------

const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";

/// Four data bits per character plus a continuation bit, written with base36
/// digits so the payload is a bare word in the source — no quoting, no
/// escaping, and `parseInt(c, 36)` decodes a character on the other side.
///
/// Only 32 of the 36 digits are used. Base64 would carry five data bits and
/// measured ~5% smaller raw, but it compresses slightly worse and has to be
/// escaped inside a JS string.
pub fn encode_ints(vals: &[i32]) -> String {
    let mut out = String::with_capacity(vals.len() * 2);
    for v in vals {
        // Zigzag, so a small negative costs as little as a small positive.
        let mut u = ((*v as i64) << 1 ^ (*v as i64) >> 63) as u64;
        loop {
            let mut d = (u & 15) as usize;
            u >>= 4;
            if u != 0 {
                d |= 16;
            }
            out.push(DIGITS[d] as char);
            if u == 0 {
                break;
            }
        }
    }
    out
}

/// Inverse of [`encode_ints`], for tests.
pub fn decode_ints(s: &str) -> Vec<i32> {
    let mut out = Vec::new();
    let (mut acc, mut sh) = (0u64, 0u32);
    for c in s.bytes() {
        let d = match c {
            b'0'..=b'9' => (c - b'0') as u64,
            _ => (c - b'a') as u64 + 10,
        };
        acc |= (d & 15) << sh;
        if d & 16 != 0 {
            sh += 4;
        } else {
            out.push(((acc >> 1) as i64 ^ -((acc & 1) as i64)) as i32);
            acc = 0;
            sh = 0;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vlq_round_trips_the_interesting_magnitudes() {
        let vals: Vec<i32> = vec![
            0, 1, -1, 15, 16, -16, 255, -255, 1000, -1000, 65535, -65536,
            i32::MAX, i32::MIN + 1, i32::MIN,
        ];
        assert_eq!(decode_ints(&encode_ints(&vals)), vals);
    }

    #[test]
    fn vlq_round_trips_a_long_sweep() {
        let vals: Vec<i32> = (0..5000).map(|i| (i * 2654435761u64 as i64 % 200003) as i32 - 100000).collect();
        assert_eq!(decode_ints(&encode_ints(&vals)), vals);
    }

    #[test]
    fn a_column_of_whole_numbers_needs_no_shift() {
        assert_eq!(shift_for([0.0, 25.0, 40.0]), 0);
        assert_eq!(shift_for([0.5, 1.0]), 1);
        assert_eq!(shift_for([0.25]), 2);
        assert_eq!(shift_for([0.125]), 3);
        // 3dp is the ceiling, because `q` has already rounded to it...
        assert_eq!(shift_for([0.001]), 3);
        // ...which is also why anything under the quantum is plain zero and
        // needs no shift at all.
        assert_eq!(shift_for([0.0001]), 0);
        // A column takes the shift its most demanding member needs.
        assert_eq!(shift_for([3.0, 0.5, 12.0]), 1);
    }

    #[test]
    fn identical_properties_share_one_offset() {
        let mut f = Flat::new();
        let a = f.prop(&Prop::Vector(vec![1.0, 2.0]));
        let b = f.prop(&Prop::Vector(vec![1.0, 2.0]));
        let c = f.prop(&Prop::Vector(vec![1.0, 3.0]));
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn no_property_lands_on_the_absent_offset() {
        let mut f = Flat::new();
        assert_ne!(f.prop(&Prop::Scalar(0.0)), 0);
        assert_eq!(f.opt_prop(None), 0);
    }

    #[test]
    fn a_scalar_round_trips_through_the_stream() {
        let mut f = Flat::new();
        let off = f.prop(&Prop::Scalar(1.5)) as usize;
        let s = decode_ints(&f.encode());
        assert_eq!(s[off] & 7, tag::SCALAR);
        let shift = (s[off] >> 3) & 3;
        assert_eq!(shift, 1);
        assert_eq!(s[off + 1] as f64 / POW10[shift as usize], 1.5);
    }

    #[test]
    fn an_overflowing_value_is_reported_rather_than_truncated() {
        let mut f = Flat::new();
        f.prop(&Prop::Scalar(3e9));
        assert!(f.overflow().is_some());
    }
}
