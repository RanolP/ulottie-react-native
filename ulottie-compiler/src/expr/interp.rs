//! Frame-aware expression interpretation — the initial-frame bake's engine.
//!
//! [`super::fold`] reads a body against what the compiler knows *statically*;
//! the runtime (`runtime/expr.js`) reads the rewritten body at a frame. This
//! module is the meeting point: it evaluates the **raw** body — the After
//! Effects vocabulary of `thisComp.layer('wire').toComp(p)`, `effect(…)`,
//! `loopOut('cycle')`, `value` — at one frame, against the layer table the
//! planner built. Every numeric rule mirrors `runtime/expr.js`, which is the
//! oracle: `toComp`'s parent walk, the `ARC = 300` arc-length table, the
//! centered-difference velocity, the `zip` broadcast of `sum`/`sub`/`mul`/`div`,
//! JS truthiness.
//!
//! The contract is the same one-directional rule as `fold`: any construct the
//! interpreter does not understand yields `None` — "cannot decide" — and the
//! caller falls back to the property's own value. A missed evaluation costs a
//! slightly wrong first frame; a wrong one is a silent render change. So a
//! missing effect, an ambiguous layer name, a value shape no binding accepts:
//! all refuse rather than guess.
//!
//! What the runtime resolves by name at mount time (`thisComp.layer('…')`) is
//! resolved here against the same record table, scoped the same way
//! (`backend::layers::Index` is the reference for both). Layer controls —
//! effect parameters of type 10 holding a composition index — become layer
//! records, exactly as `lyAt`/`lyRel` spelled them after the rewrite.

use std::collections::HashMap;

use oxc_allocator::Allocator;
use oxc_ast::ast;
use oxc_parser::Parser;
use oxc_span::SourceType;

use crate::scene::LayerRecord;
use crate::scene::prop::{Anim, AnimKind, Prop};

// ---------------------------------------------------------------------------
// Values
// ---------------------------------------------------------------------------

/// `(position, anchor, scale, rotation)` — what a space walk composes per level.
type LocalTransform = ([f64; 2], [f64; 2], [f64; 2], f64);

/// A value as the expression engine sees it at runtime.
///
/// Arrays are JS arrays: a flat numeric vector is an [`Value::Arr`] of
/// [`Value::Num`], and the pair lists the path accessors return are an `Arr`
/// of two-element `Arr`s. The arithmetic helpers broadcast over whichever
/// they are given, so the one representation serves both.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Num(f64),
    Bool(bool),
    Str(String),
    Arr(Vec<Value>),
    Path(PathVal),
    /// A layer record index into `Site::layers`.
    Layer(u32),
    /// `layer.effect(…)` / bare `effect(…)`, before a parameter is selected.
    EffectSel {
        rec: u32,
        effect: usize,
    },
    /// `thisProperty.propertyGroup(n)`; selecting off it searches the owning
    /// layer's effects (the same search `fold` performs).
    Group,
    ThisProp,
    /// One keyframe: `key(n)` / `nearestKey(t)`.
    Key {
        index: f64,
        time: f64,
        value: Box<Value>,
    },
    Comp,
    Math,
    Null,
    Undefined,
}

/// A bezier path as the path API holds it: flat coordinate pairs plus a
/// closed flag. The same shape `runtime/expr.js` caches its arc table on.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PathVal {
    pub v: Vec<f64>,
    pub i: Vec<f64>,
    pub o: Vec<f64>,
    pub c: bool,
}

impl Value {
    /// Numeric coercion, for the shapes that have one.
    fn num(&self) -> Option<f64> {
        match self {
            Value::Num(n) => Some(*n),
            Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            _ => None,
        }
    }

    /// JavaScript truthiness. Every value here has one.
    fn truthy(&self) -> bool {
        match self {
            Value::Bool(b) => *b,
            Value::Num(n) => *n != 0.0 && !n.is_nan(),
            Value::Str(s) => !s.is_empty(),
            // JS: any object — and any array, even empty — is truthy.
            Value::Null | Value::Undefined => false,
            _ => true,
        }
    }

    /// A flat numeric copy, for the helpers that want a vector.
    fn flat_nums(&self) -> Option<Vec<f64>> {
        match self {
            Value::Arr(items) => items.iter().map(|v| v.num()).collect(),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// The site, and how the interpreter reads properties
// ---------------------------------------------------------------------------

/// Everything one evaluation runs against: the frame, the owning record, the
/// value source `value` reads, and the planner's layer table.
pub struct Site<'a> {
    pub frame: f64,
    pub fr: f64,
    /// The record `thisLayer` resolves to; `None` when the property has no
    /// owning layer, which makes any body that reaches for one refuse.
    pub owner: Option<u32>,
    /// The property's own value source — what `value` reads.
    pub fallback: Option<&'a Prop>,
    pub layers: &'a [LayerRecord],
    pub scopes: &'a [u32],
    pub names: &'a [String],
}

/// How the interpreter reads a scene property at a frame — the planner's own
/// `value_at`, expression properties included (which is how one body reading
/// another layer's expression-driven property recurses, under the caller's
/// depth guard).
pub trait Host {
    fn prop_at(&self, p: &Prop, f: f64) -> Option<Value>;
}

/// Evaluate `body` at `site.frame`. `None` means "cannot decide".
pub fn eval_at(body: &str, site: &Site, host: &dyn Host) -> Option<Value> {
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, body, SourceType::cjs()).parse();
    if !parsed.errors.is_empty() {
        return None;
    }

    let mut env = Env {
        vars: HashMap::new(),
        site,
        host,
    };
    for stmt in &parsed.program.body {
        match env.exec(stmt)? {
            Flow::Normal => {}
            Flow::Return(v) => return Some(v),
        }
    }
    // Bodymovin assigns the result to `$bm_rt`; the emitter returns it.
    env.vars.get("$bm_rt").cloned()
}

enum Flow {
    Normal,
    Return(Value),
}

/// Iteration bound for the loops bodies actually run (the `nullLayerNames`
/// walks). A body that needs more than this is not one After Effects emits.
const LOOP_MAX: usize = 10_000;

struct Env<'a, 's> {
    vars: HashMap<String, Value>,
    site: &'s Site<'a>,
    host: &'s dyn Host,
}

/// What `thisProperty` is, decided the way `expr::thisPropertyFor` decides it:
/// from the value source's shape. The path is owned because a static path
/// source is read straight off the fallback's literal.
enum Surface {
    Keyed(Box<Anim>),
    Path(PathVal),
    Stub,
}

// ---------------------------------------------------------------------------
// Statements
// ---------------------------------------------------------------------------

impl Env<'_, '_> {
    /// `None` means "cannot decide" and aborts the whole evaluation.
    fn exec(&mut self, stmt: &ast::Statement) -> Option<Flow> {
        match stmt {
            ast::Statement::VariableDeclaration(d) => {
                self.var_decl(d)?;
                Some(Flow::Normal)
            }
            ast::Statement::ExpressionStatement(s) => {
                self.eval(&s.expression)?;
                Some(Flow::Normal)
            }
            ast::Statement::BlockStatement(b) => self.block(&b.body),
            ast::Statement::IfStatement(s) => {
                if self.eval(&s.test)?.truthy() {
                    self.exec(&s.consequent)
                } else {
                    match &s.alternate {
                        Some(alt) => self.exec(alt),
                        None => Some(Flow::Normal),
                    }
                }
            }
            ast::Statement::ReturnStatement(s) => match &s.argument {
                Some(e) => Some(Flow::Return(self.eval(e)?)),
                None => Some(Flow::Return(Value::Undefined)),
            },
            ast::Statement::EmptyStatement(_) => Some(Flow::Normal),
            // A `try` whose body evaluates cleanly cannot throw, so the catch
            // is dead — the same rule `fold` applies. A body that does not
            // evaluate refuses wholesale: the catch is not a licence to guess.
            ast::Statement::TryStatement(s) => self.block(&s.block.body),
            ast::Statement::ForStatement(f) => self.for_loop(f),
            _ => None,
        }
    }

    fn var_decl(&mut self, d: &ast::VariableDeclaration) -> Option<()> {
        for decl in &d.declarations {
            let name = decl.id.get_binding_identifier()?.name.as_str().to_string();
            match &decl.init {
                None => {
                    self.vars.entry(name).or_insert(Value::Undefined);
                }
                Some(e) => {
                    let v = self.eval(e)?;
                    self.vars.insert(name, v);
                }
            }
        }
        Some(())
    }

    fn block(&mut self, stmts: &[ast::Statement]) -> Option<Flow> {
        for s in stmts {
            match self.exec(s)? {
                Flow::Normal => {}
                r => return Some(r),
            }
        }
        Some(Flow::Normal)
    }

    fn for_loop(&mut self, f: &ast::ForStatement) -> Option<Flow> {
        if let Some(init) = &f.init {
            match init {
                ast::ForStatementInit::VariableDeclaration(d) => self.var_decl(d)?,
                // An expression initializer is legal JS none of these bodies
                // use; refuse rather than convert between the enums.
                _ => return None,
            }
        }
        // `for (;;)` has no test, and no exit this interpreter could find.
        let Some(test) = &f.test else { return None };
        let mut steps = 0usize;
        loop {
            if !self.eval(test)?.truthy() {
                return Some(Flow::Normal);
            }
            match self.exec(&f.body)? {
                Flow::Normal => {}
                r => return Some(r),
            }
            if let Some(u) = &f.update {
                self.eval(u)?;
            }
            steps += 1;
            if steps > LOOP_MAX {
                return None;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Expressions
// ---------------------------------------------------------------------------

impl Env<'_, '_> {
    fn eval(&mut self, e: &ast::Expression) -> Option<Value> {
        match e {
            ast::Expression::NumericLiteral(n) => Some(Value::Num(n.value)),
            ast::Expression::BooleanLiteral(b) => Some(Value::Bool(b.value)),
            ast::Expression::StringLiteral(s) => Some(Value::Str(s.value.as_str().to_string())),
            ast::Expression::NullLiteral(_) => Some(Value::Null),
            ast::Expression::Identifier(id) => self.ident(id.name.as_str()),
            ast::Expression::ParenthesizedExpression(p) => self.eval(&p.expression),
            ast::Expression::SequenceExpression(s) => {
                let mut last = Value::Undefined;
                for e in &s.expressions {
                    last = self.eval(e)?;
                }
                Some(last)
            }
            ast::Expression::ArrayExpression(a) => {
                let mut out = Vec::with_capacity(a.elements.len());
                for el in &a.elements {
                    out.push(self.eval(el.as_expression()?)?);
                }
                Some(Value::Arr(out))
            }
            ast::Expression::AssignmentExpression(a) => self.assign(a),
            ast::Expression::UpdateExpression(u) => self.update(u),
            ast::Expression::UnaryExpression(u) => {
                let v = self.eval(&u.argument)?;
                match u.operator {
                    ast::UnaryOperator::UnaryNegation => Some(Value::Num(-v.num()?)),
                    ast::UnaryOperator::UnaryPlus => Some(Value::Num(v.num()?)),
                    ast::UnaryOperator::LogicalNot => Some(Value::Bool(!v.truthy())),
                    _ => None,
                }
            }
            ast::Expression::BinaryExpression(b) => {
                let l = self.eval(&b.left)?;
                let r = self.eval(&b.right)?;
                binary(b.operator, &l, &r)
            }
            ast::Expression::LogicalExpression(l) => {
                let left = self.eval(&l.left)?;
                match l.operator {
                    ast::LogicalOperator::And => {
                        if !left.truthy() {
                            Some(left)
                        } else {
                            self.eval(&l.right)
                        }
                    }
                    ast::LogicalOperator::Or => {
                        if left.truthy() {
                            Some(left)
                        } else {
                            self.eval(&l.right)
                        }
                    }
                    ast::LogicalOperator::Coalesce => None,
                }
            }
            ast::Expression::ConditionalExpression(c) => {
                if self.eval(&c.test)?.truthy() {
                    self.eval(&c.consequent)
                } else {
                    self.eval(&c.alternate)
                }
            }
            ast::Expression::StaticMemberExpression(m) => {
                let obj = self.eval(&m.object)?;
                self.member(&obj, m.property.name.as_str())
            }
            ast::Expression::ComputedMemberExpression(m) => {
                let obj = self.eval(&m.object)?;
                let key = self.eval(&m.expression)?;
                self.index(&obj, &key)
            }
            ast::Expression::CallExpression(c) => self.call(c),
            _ => None,
        }
    }

    fn assign(&mut self, a: &ast::AssignmentExpression) -> Option<Value> {
        // Plain `x = v` stores as-is; compound reads the old value first.
        let op = a.operator.to_binary_operator();
        match &a.left {
            // The simple-identifier form is the only one bodies write to a
            // name; everything interesting on the left is a member store.
            t if t.get_identifier_name().is_some() => {
                let name = t.get_identifier_name()?.to_string();
                let v = self.eval(&a.right)?;
                let v = match op {
                    None => v,
                    Some(op) => {
                        let old = self.vars.get(&name).cloned().unwrap_or(Value::Undefined);
                        binary(op, &old, &v)?
                    }
                };
                self.vars.insert(name, v.clone());
                Some(v)
            }
            ast::AssignmentTarget::ComputedMemberExpression(m) => {
                // `points[i] = p` — the createPath pattern. The object is a
                // variable in every body that does this.
                let ast::Expression::Identifier(id) = &m.object else {
                    return None;
                };
                let name = id.name.as_str().to_string();
                let i = self.eval(&m.expression)?.num()? as usize;
                let v = self.eval(&a.right)?;
                let Value::Arr(items) = self.vars.get(&name).cloned().unwrap_or(Value::Undefined)
                else {
                    return None;
                };
                let mut items = items;
                if i >= items.len() {
                    // JS would grow the array, punching holes this value
                    // shape cannot express; refuse rather than drop the write.
                    return None;
                }
                items[i] = v.clone();
                self.vars.insert(name, Value::Arr(items));
                Some(v)
            }
            _ => None,
        }
    }

    fn update(&mut self, u: &ast::UpdateExpression) -> Option<Value> {
        let ast::SimpleAssignmentTarget::AssignmentTargetIdentifier(id) = &u.argument else {
            return None;
        };
        let name = id.name.as_str();
        let old = self
            .vars
            .get(name)
            .cloned()
            .unwrap_or(Value::Undefined)
            .num()?;
        let next = match u.operator {
            ast::UpdateOperator::Increment => old + 1.0,
            ast::UpdateOperator::Decrement => old - 1.0,
        };
        self.vars.insert(name.to_string(), Value::Num(next));
        Some(Value::Num(if u.prefix { next } else { old }))
    }

    // -- identifiers ---------------------------------------------------------

    fn ident(&self, name: &str) -> Option<Value> {
        if let Some(v) = self.vars.get(name) {
            return Some(v.clone());
        }
        match name {
            "value" => Some(self.base_value()),
            "thisProperty" => Some(Value::ThisProp),
            "thisLayer" => self.site.owner.map(Value::Layer),
            "thisComp" => Some(Value::Comp),
            "numKeys" => Some(Value::Num(self.num_keys() as f64)),
            "time" => Some(Value::Num(self.site.frame / self.site.fr)),
            "frameDuration" => Some(Value::Num(1.0 / self.site.fr)),
            "Math" => Some(Value::Math),
            _ => None,
        }
    }

    /// What the expression sees as `value`: the property's own source at the
    /// current frame. `readProp(null, frame, 0)` answers 0.
    fn base_value(&self) -> Value {
        self.site
            .fallback
            .and_then(|p| self.host.prop_at(p, self.site.frame))
            .unwrap_or(Value::Num(0.0))
    }

    fn surface(&self) -> Surface {
        match self.site.fallback {
            Some(Prop::Path(p)) => Surface::Path(PathVal {
                v: p.v.clone(),
                i: p.i.clone(),
                o: p.o.clone(),
                c: p.c,
            }),
            Some(Prop::Anim(a)) if a.kind != AnimKind::Path => Surface::Keyed(a.clone()),
            _ => Surface::Stub,
        }
    }

    fn num_keys(&self) -> usize {
        match self.surface() {
            Surface::Keyed(a) => a.t.len(),
            _ => 0,
        }
    }

    // -- member access -------------------------------------------------------

    fn member(&self, obj: &Value, prop: &str) -> Option<Value> {
        match (obj, prop) {
            (Value::Arr(items), "length") => Some(Value::Num(items.len() as f64)),
            (Value::Str(s), "length") => Some(Value::Num(s.encode_utf16().count() as f64)),
            (Value::Math, "PI") => Some(Value::Num(std::f64::consts::PI)),
            (Value::Math, "E") => Some(Value::Num(std::f64::consts::E)),
            (Value::Comp, "frameDuration") => Some(Value::Num(1.0 / self.site.fr)),
            (Value::ThisProp, "numKeys") => Some(Value::Num(self.num_keys() as f64)),
            (Value::Key { value, .. }, "value") => Some((**value).clone()),
            (Value::Key { index, .. }, "index") => Some(Value::Num(*index)),
            (Value::Key { time, .. }, "time") => Some(Value::Num(*time)),
            (Value::Layer(rec), _) => self.layer_member(*rec, prop),
            _ => None,
        }
    }

    fn index(&self, obj: &Value, key: &Value) -> Option<Value> {
        match obj {
            Value::Arr(items) => {
                // JS converts the key to an index; only integers address
                // elements, anything else reads a missing property.
                let i = key.num()?;
                if i.fract() != 0.0 || i < 0.0 || i as usize >= items.len() {
                    return Some(Value::Undefined);
                }
                Some(items[i as usize].clone())
            }
            Value::Str(s) => {
                let i = key.num()?;
                if i.fract() != 0.0 {
                    return Some(Value::Undefined);
                }
                s.encode_utf16()
                    .nth(i as usize)
                    .map(|c| Value::Str(String::from_utf16_lossy(&[c])))
            }
            _ => None,
        }
    }

    // -- layer ---------------------------------------------------------------

    /// A static member read on a layer record. Mirrors the free functions the
    /// rewrite emits (`lyPos`, `lyAnchor`, …) and the proxy behind them.
    fn layer_member(&self, rec: u32, prop: &str) -> Option<Value> {
        let r = self.site.layers.get(rec as usize)?;
        match prop {
            "transform" | "content" => Some(Value::Layer(rec)),
            "parentLayer" => match r.pr {
                Some(p) => Some(Value::Layer(p)),
                None => Some(Value::Undefined),
            },
            "index" => Some(Value::Num(r.i as f64)),
            "name" => match r.n.and_then(|n| self.site.names.get(n as usize)) {
                Some(n) => Some(Value::Str(n.clone())),
                None => Some(Value::Null),
            },
            // A chain that *ends* at `.path` is the layer's first path shape —
            // `lyPath`, not the record.
            "path" => {
                r.h.as_ref()
                    .and_then(|p| self.host.prop_at(p, self.site.frame))
            }
            "position" => self.field_or(&r.p, &[0.0, 0.0, 0.0]),
            "anchorPoint" => self.field_or(&r.a, &[0.0, 0.0, 0.0]),
            "scale" => self.field_or(&r.sc, &[100.0, 100.0, 100.0]),
            "rotation" => Some(match &r.r {
                None => Value::Num(0.0),
                Some(p) => self.host.prop_at(p, self.site.frame)?,
            }),
            "opacity" => Some(match &r.o {
                None => Value::Num(100.0),
                Some(p) => self.host.prop_at(p, self.site.frame)?,
            }),
            _ => None,
        }
    }

    /// A field read whose absent form is a default vector (`readProp`'s
    /// fallback literal — opacity is 100, not 0).
    fn field_or(&self, p: &Option<Prop>, default: &[f64]) -> Option<Value> {
        match p {
            None => Some(Value::Arr(default.iter().map(|n| Value::Num(*n)).collect())),
            Some(p) => self.host.prop_at(p, self.site.frame),
        }
    }

    // -- calls ---------------------------------------------------------------

    fn call(&mut self, c: &ast::CallExpression) -> Option<Value> {
        // `arr.push(x)` — a store, not a read, so it is matched on the callee
        // shape before the object is evaluated to a value.
        if let ast::Expression::StaticMemberExpression(m) = &c.callee
            && m.property.name == "push"
            && let ast::Expression::Identifier(id) = &m.object
            && matches!(self.vars.get(id.name.as_str()), Some(Value::Arr(_)) | None)
        {
            let mut items = match self.vars.get(id.name.as_str()).cloned() {
                Some(Value::Arr(items)) => items,
                // Declared empty, the one way bodies build these.
                _ => Vec::new(),
            };
            for a in &c.arguments {
                items.push(self.eval(a.as_expression()?)?);
            }
            let len = items.len() as f64;
            self.vars
                .insert(id.name.as_str().to_string(), Value::Arr(items));
            return Some(Value::Num(len));
        }

        let mut args = Vec::with_capacity(c.arguments.len());
        for a in &c.arguments {
            args.push(self.eval(a.as_expression()?)?);
        }

        match &c.callee {
            ast::Expression::StaticMemberExpression(m) => {
                let prop = m.property.name.as_str();
                if let ast::Expression::Identifier(id) = &m.object
                    && id.name == "Math"
                {
                    return math(prop, &args);
                }
                let obj = self.eval(&m.object)?;
                self.method_call(obj, prop, args)
            }
            ast::Expression::Identifier(id) => {
                let name = id.name.as_str();
                // A variable holding a callable value (`pathLayer('ADBE …')`).
                if let Some(v) = self.vars.get(name).cloned() {
                    return self.value_call(v, args);
                }
                self.free_call(name, args)
            }
            _ => {
                let callee = self.eval(&c.callee)?;
                self.value_call(callee, args)
            }
        }
    }

    /// Calling a value directly: parameter selection off an effect, the
    /// property-group drill, and the layer drill chains.
    fn value_call(&self, callee: Value, args: Vec<Value>) -> Option<Value> {
        match callee {
            Value::EffectSel { rec, effect } => {
                let sel = args.first()?;
                self.param_value(rec, effect, sel)
            }
            Value::Group => match args.first() {
                Some(Value::Str(key)) => self.sibling(key),
                None => Some(Value::Bool(true)),
                _ => None,
            },
            // The proxy answered itself, so every drill-down link collapses
            // back to the layer — the record is what the chain resolves to.
            Value::Layer(rec) => Some(Value::Layer(rec)),
            _ => None,
        }
    }

    /// A call named as a member on a value.
    fn method_call(&self, obj: Value, name: &str, args: Vec<Value>) -> Option<Value> {
        match obj {
            Value::Comp if name == "layer" => {
                let owner = self.site.owner?;
                match args.first()? {
                    Value::Str(n) => self
                        .find_layer(owner, |r| {
                            r.n.and_then(|i| self.site.names.get(i as usize)) == Some(n)
                        })
                        .map(Value::Layer),
                    Value::Num(i) if i.fract() == 0.0 && *i >= 0.0 => self
                        .find_layer(owner, |r| r.i == *i as u32)
                        .map(Value::Layer),
                    _ => None,
                }
            }
            Value::Layer(rec) => self.layer_method(rec, name, &args),
            Value::ThisProp => self.prop_method(name, &args),
            Value::Path(p) => self.path_method(&p, name, &args),
            _ => None,
        }
    }

    fn layer_method(&self, rec: u32, name: &str, args: &[Value]) -> Option<Value> {
        match name {
            "toComp" => self.to_comp(rec, args.first()?),
            "fromCompToSurface" => self.from_comp_to_surface(rec, args.first()?),
            "effect" => {
                let r = self.site.layers.get(rec as usize)?;
                let idx = self.select_effect(r, args.first()?)?;
                Some(Value::EffectSel { rec, effect: idx })
            }
            "content" => Some(Value::Layer(rec)),
            "pointOnPath" | "tangentOnPath" => {
                let u = args.first()?.num()?;
                let path = self.layer_path(rec);
                Some(if name == "pointOnPath" {
                    point_on_path(path.as_ref(), u)
                } else {
                    tangent_on_path(path.as_ref(), u)
                })
            }
            "points" | "inTangents" | "outTangents" => {
                let path = self.layer_path(rec)?;
                Some(pairs(
                    &path,
                    match name {
                        "inTangents" => Some("i"),
                        "outTangents" => Some("o"),
                        _ => None,
                    },
                ))
            }
            "isClosed" => Some(Value::Bool(self.layer_path(rec)?.c)),
            _ => None,
        }
    }

    /// The layer's first path shape, or `None` for a layer without one — the
    /// `lyPath` record read, null in the runtime.
    fn layer_path(&self, rec: u32) -> Option<PathVal> {
        let r = self.site.layers.get(rec as usize)?;
        match r
            .h
            .as_ref()
            .and_then(|p| self.host.prop_at(p, self.site.frame))?
        {
            Value::Path(p) => Some(p),
            _ => None,
        }
    }

    /// `thisProperty.<method>` — the keyed / path / stub surfaces of
    /// `thisPropertyFor`, accessor for accessor.
    fn prop_method(&self, name: &str, args: &[Value]) -> Option<Value> {
        let surface = self.surface();
        match name {
            "propertyGroup" => Some(Value::Group),
            "points" | "inTangents" | "outTangents" | "isClosed" => {
                let Surface::Path(p) = &surface else {
                    return None;
                };
                match name {
                    "points" => Some(pairs(p, None)),
                    "inTangents" => Some(pairs(p, Some("i"))),
                    "outTangents" => Some(pairs(p, Some("o"))),
                    _ => Some(Value::Bool(p.c)),
                }
            }
            "numKeys" => Some(Value::Num(self.num_keys() as f64)),
            "loopOut" => match &surface {
                Surface::Keyed(a) => self.loop_out(a, args.first()),
                Surface::Path(_) => None,
                Surface::Stub => Some(self.base_value()),
            },
            "valueAtTime" => match &surface {
                Surface::Keyed(_) => {
                    let t = args.first()?.num()?;
                    self.site
                        .fallback
                        .and_then(|p| self.host.prop_at(p, t * self.site.fr))
                }
                // The stub ignores the argument and answers the current value.
                Surface::Stub => Some(self.base_value()),
                Surface::Path(_) => None,
            },
            "velocityAtTime" => match &surface {
                Surface::Keyed(_) => {
                    let t = args.first()?.num()?;
                    self.velocity_at(t)
                }
                Surface::Stub => Some(Value::Num(0.0)),
                Surface::Path(_) => None,
            },
            "key" => match &surface {
                Surface::Keyed(a) => {
                    let i = args.first()?.num()?;
                    self.key_of(a, i)
                }
                Surface::Stub => {
                    let i = args.first()?.num()?;
                    Some(Value::Key {
                        index: i,
                        time: 0.0,
                        value: Box::new(self.base_value()),
                    })
                }
                Surface::Path(_) => None,
            },
            "nearestKey" => match &surface {
                Surface::Keyed(a) => {
                    let tf = args.first()?.num()? * self.site.fr;
                    let mut best = 0usize;
                    let mut dist = f64::INFINITY;
                    for (i, t) in a.t.iter().enumerate() {
                        let d = (t - tf).abs();
                        if d < dist {
                            dist = d;
                            best = i;
                        }
                    }
                    Some(Value::Key {
                        index: (best + 1) as f64,
                        time: a.t[best] / self.site.fr,
                        value: Box::new(key_value(a, best)),
                    })
                }
                Surface::Stub => Some(Value::Key {
                    index: 1.0,
                    time: 0.0,
                    value: Box::new(Value::Undefined),
                }),
                Surface::Path(_) => None,
            },
            _ => None,
        }
    }

    fn key_of(&self, a: &Anim, i: f64) -> Option<Value> {
        let n = a.t.len();
        if n == 0 {
            return None;
        }
        let k = (i as isize - 1).clamp(0, n as isize - 1) as usize;
        Some(Value::Key {
            index: i,
            time: a.t[k] / self.site.fr,
            value: Box::new(key_value(a, k)),
        })
    }

    /// `loopOut(mode)` — the cycle / pingpong table from `keyedProperty`.
    fn loop_out(&self, a: &Anim, mode: Option<&Value>) -> Option<Value> {
        let at =
            |f: f64| -> Option<Value> { self.site.fallback.and_then(|p| self.host.prop_at(p, f)) };
        let (t0, tn) = (*a.t.first()?, *a.t.last()?);
        let span = tn - t0;
        if span <= 0.0 {
            return at(t0);
        }
        let f = self.site.frame;
        if f <= tn {
            return at(f);
        }
        let past = f - tn;
        let mode = match mode {
            Some(Value::Str(s)) => s.as_str(),
            _ => "",
        };
        if mode == "pingpong" || mode == "pingPong" {
            let cycles = (past / span).floor();
            let r = past - cycles * span;
            return at(if cycles % 2.0 == 0.0 { tn - r } else { t0 + r });
        }
        at(t0 + (past - (past / span).floor() * span))
    }

    /// Centered-difference velocity, `dt = 0.001` — the same 0.001 the runtime
    /// uses, including its better match to the reference at `t = 0`.
    fn velocity_at(&self, time: f64) -> Option<Value> {
        let dt = 0.001;
        let at = |t: f64| -> Option<Value> {
            self.site
                .fallback
                .and_then(|p| self.host.prop_at(p, t * self.site.fr))
        };
        let a = at(time - dt / 2.0)?;
        let b = at(time + dt / 2.0)?;
        let inv = 1.0 / dt;
        match (a, b) {
            (Value::Num(x), Value::Num(y)) => Some(Value::Num((y - x) * inv)),
            (Value::Arr(x), Value::Arr(y)) => {
                if x.len() != y.len() {
                    return None;
                }
                Some(Value::Arr(
                    x.into_iter()
                        .zip(y)
                        .map(|(x, y)| Some(Value::Num((y.num()? - x.num()?) * inv)))
                        .collect::<Option<Vec<_>>>()?,
                ))
            }
            _ => None,
        }
    }

    fn path_method(&self, p: &PathVal, name: &str, args: &[Value]) -> Option<Value> {
        match name {
            "points" => Some(pairs(p, None)),
            "inTangents" => Some(pairs(p, Some("i"))),
            "outTangents" => Some(pairs(p, Some("o"))),
            "isClosed" => Some(Value::Bool(p.c)),
            "pointOnPath" => Some(point_on_path(Some(p), args.first()?.num()?)),
            "tangentOnPath" => Some(tangent_on_path(Some(p), args.first()?.num()?)),
            _ => None,
        }
    }

    // -- free functions ------------------------------------------------------

    fn free_call(&self, name: &str, args: Vec<Value>) -> Option<Value> {
        // The `thisProperty` surface methods also ship as bare names — the
        // preamble binds them off the property, and `loopOut('cycle')` is the
        // whole body of more than one fixture property.
        if matches!(
            name,
            "loopOut" | "key" | "nearestKey" | "valueAtTime" | "velocityAtTime"
        ) {
            return self.prop_method(name, &args);
        }
        match name {
            "effect" => {
                let owner = self.site.owner?;
                let r = self.site.layers.get(owner as usize)?;
                let idx = self.select_effect(r, args.first()?)?;
                Some(Value::EffectSel {
                    rec: owner,
                    effect: idx,
                })
            }
            // Bare `fromCompToSurface(pt)` is the owning layer's inverse.
            "fromCompToSurface" if args.len() == 1 => {
                let owner = self.site.owner?;
                self.from_comp_to_surface(owner, args.first()?)
            }
            "sum" | "add" => zip(&args, 0.0, |a, b| a + b),
            "sub" => zip(&args, 0.0, |a, b| a - b),
            "mul" => zip(&args, 1.0, |a, b| a * b),
            "div" => zip(&args, 1.0, |a, b| a / b),
            "clamp" => {
                let (v, lo, hi) = (args.first()?, args.get(1)?.num()?, args.get(2)?.num()?);
                match v {
                    Value::Arr(items) => Some(Value::Arr(
                        items
                            .iter()
                            .map(|x| Some(Value::Num(x.num()?.clamp(lo, hi))))
                            .collect::<Option<Vec<_>>>()?,
                    )),
                    v => Some(Value::Num(v.num()?.clamp(lo, hi))),
                }
            }
            "radiansToDegrees" => Some(Value::Num(
                args.first()?.num()? * 180.0 / std::f64::consts::PI,
            )),
            "degreesToRadians" => Some(Value::Num(
                args.first()?.num()? * std::f64::consts::PI / 180.0,
            )),
            "pointOnPath" => {
                let u = args.get(1)?.num()?;
                let path = match args.first()? {
                    Value::Path(p) => Some(p),
                    Value::Null | Value::Undefined => None,
                    _ => return None,
                };
                Some(point_on_path(path, u))
            }
            "tangentOnPath" => {
                let u = args.get(1)?.num()?;
                let path = match args.first()? {
                    Value::Path(p) => Some(p),
                    Value::Null | Value::Undefined => None,
                    _ => return None,
                };
                Some(tangent_on_path(path, u))
            }
            "createPath" => {
                create_path(args.first(), args.get(1), args.get(2), args.get(3)).map(Value::Path)
            }
            "length" => match args.first()? {
                Value::Arr(_) => {
                    let nums = args.first()?.flat_nums()?;
                    Some(Value::Num(nums.iter().map(|x| x * x).sum::<f64>().sqrt()))
                }
                v => Some(Value::Num(v.num()?.abs())),
            },
            "normalize" => {
                let nums = match args.first()? {
                    Value::Arr(_) => args.first()?.flat_nums()?,
                    _ => return None,
                };
                let len = nums.iter().map(|x| x * x).sum::<f64>().sqrt();
                if len == 0.0 {
                    return None;
                }
                Some(Value::Arr(
                    nums.iter().map(|x| Value::Num(x / len)).collect(),
                ))
            }
            _ => None,
        }
    }

    // -- effects -------------------------------------------------------------

    /// One effect slot off a record, by index or by display / match name.
    /// The runtime takes the first match; so does this.
    fn select_effect(&self, r: &LayerRecord, sel: &Value) -> Option<usize> {
        match sel {
            Value::Num(i) if i.fract() == 0.0 && *i >= 0.0 => {
                ((*i as usize) < r.ef.len()).then_some(*i as usize)
            }
            Value::Str(s) => r.ef.iter().position(|e| {
                e.nm.as_deref() == Some(s.as_str()) || e.mn.as_deref() == Some(s.as_str())
            }),
            _ => None,
        }
    }

    fn select_param(&self, e: &crate::scene::Effect, sel: &Value) -> Option<usize> {
        match sel {
            Value::Num(i) if i.fract() == 0.0 && *i >= 0.0 => {
                ((*i as usize) < e.ef.len()).then_some(*i as usize)
            }
            Value::Str(s) => e.ef.iter().position(|p| {
                p.nm.as_deref() == Some(s.as_str()) || p.mn.as_deref() == Some(s.as_str())
            }),
            _ => None,
        }
    }

    /// `X.effect(name)(param)` — the parameter's value, or the layer a
    /// type-10 layer control names.
    fn param_value(&self, rec: u32, effect: usize, sel: &Value) -> Option<Value> {
        let r = self.site.layers.get(rec as usize)?;
        let e = r.ef.get(effect)?;
        let p = self.select_param(e, sel)?;
        self.param_at(rec, effect, p)
    }

    fn param_at(&self, rec: u32, effect: usize, param: usize) -> Option<Value> {
        let ep = self
            .site
            .layers
            .get(rec as usize)?
            .ef
            .get(effect)?
            .ef
            .get(param)?;
        if ep.ty == 10 {
            // A layer control holds a composition index.
            let ind = ep.v? as u32;
            return self.find_layer(rec, |r| r.i == ind).map(Value::Layer);
        }
        if let Some(v) = ep.v {
            return Some(Value::Num(v));
        }
        match &ep.p {
            Some(p) => self.host.prop_at(p, self.site.frame),
            None => Some(Value::Num(0.0)),
        }
    }

    /// A parameter reached through `propertyGroup`, which names a sibling of
    /// the property this expression is on. The same search `fold` runs, with
    /// the same refusal when more than one effect answers to the name.
    fn sibling(&self, key: &str) -> Option<Value> {
        let owner = self.site.owner?;
        let r = self.site.layers.get(owner as usize)?;
        let mut found = None;
        for (e, eff) in r.ef.iter().enumerate() {
            for p in 0..eff.ef.len() {
                let ep = &eff.ef[p];
                if ep.nm.as_deref() == Some(key) || ep.mn.as_deref() == Some(key) {
                    if found.is_some() {
                        return None;
                    }
                    found = Some(self.param_at(owner, e, p));
                }
            }
        }
        found?
    }

    // -- comp-space walks ----------------------------------------------------

    /// `thisComp.layer(…)` within the owner's composition scope. Two records
    /// answering to one key refuse rather than pick.
    fn find_layer(&self, owner: u32, pred: impl Fn(&LayerRecord) -> bool) -> Option<u32> {
        let scope = *self.site.scopes.get(owner as usize)?;
        let mut found = None;
        for (i, r) in self.site.layers.iter().enumerate() {
            if self.site.scopes.get(i) != Some(&scope) {
                continue;
            }
            if pred(r) {
                if found.is_some() {
                    return None;
                }
                found = Some(i as u32);
            }
        }
        found
    }

    /// The four fields the space walks compose, with the runtime's own
    /// `readProp` defaults already applied.
    fn local_transform(&self, rec: u32) -> Option<LocalTransform> {
        let r = self.site.layers.get(rec as usize)?;
        let vec = |p: &Option<Prop>, d: [f64; 2]| -> Option<[f64; 2]> {
            let v = match p {
                None => return Some(d),
                Some(p) => self.host.prop_at(p, self.site.frame)?,
            };
            match v {
                Value::Arr(items) if items.len() >= 2 => Some([items[0].num()?, items[1].num()?]),
                Value::Num(n) => Some([n, n]),
                _ => None,
            }
        };
        let rot = match &r.r {
            None => 0.0,
            Some(p) => match self.host.prop_at(p, self.site.frame)? {
                Value::Num(n) => n,
                Value::Arr(items) => items.first()?.num()?,
                _ => return None,
            },
        };
        Some((
            vec(&r.p, [0.0, 0.0])?,
            vec(&r.a, [0.0, 0.0])?,
            vec(&r.sc, [100.0, 100.0])?,
            rot,
        ))
    }

    fn to_comp(&self, rec: u32, pt: &Value) -> Option<Value> {
        let (mut x, mut y) = self.point2(pt)?;
        let mut layer = Some(rec);
        while let Some(l) = layer {
            let (p, a, s, r) = self.local_transform(l)?;
            let mut nx = x - js_or0(Some(a[0]));
            let mut ny = y - js_or0(Some(a[1]));
            nx *= js_or0(Some(s[0])) / 100.0;
            ny *= js_or0(Some(s[1])) / 100.0;
            if r != 0.0 {
                let rad = r.to_radians();
                let (sn, cs) = rad.sin_cos();
                (nx, ny) = (nx * cs - ny * sn, nx * sn + ny * cs);
            }
            x = nx + js_or0(Some(p[0]));
            y = ny + js_or0(Some(p[1]));
            layer = self.site.layers.get(l as usize).and_then(|r| r.pr);
        }
        Some(Value::Arr(vec![Value::Num(x), Value::Num(y)]))
    }

    /// Mirrors `fromCompToSurface` in expr.js — the name is the runtime's.
    #[allow(clippy::wrong_self_convention)]
    fn from_comp_to_surface(&self, rec: u32, pt: &Value) -> Option<Value> {
        let mut stack = Vec::new();
        let mut l = Some(rec);
        while let Some(r) = l {
            stack.push(r);
            l = self.site.layers.get(r as usize).and_then(|x| x.pr);
        }
        let (mut x, mut y) = self.point2(pt)?;
        for lyr in stack.iter().rev() {
            let (p, a, s, r) = self.local_transform(*lyr)?;
            x -= js_or0(Some(p[0]));
            y -= js_or0(Some(p[1]));
            if r != 0.0 {
                let rad = -r.to_radians();
                let (sn, cs) = rad.sin_cos();
                (x, y) = (x * cs - y * sn, x * sn + y * cs);
            }
            x *= 100.0 / js_or0(Some(s[0]));
            y *= 100.0 / js_or0(Some(s[1]));
            x += js_or0(Some(a[0]));
            y += js_or0(Some(a[1]));
        }
        Some(Value::Arr(vec![Value::Num(x), Value::Num(y)]))
    }

    /// A point argument as `(x, y)` — a flat pair or a pair array, missing
    /// components reading as 0 the way `point[0] || 0` does.
    fn point2(&self, pt: &Value) -> Option<(f64, f64)> {
        let get = |v: &Value, i: usize| -> f64 {
            match v {
                Value::Arr(items) => match items.get(i) {
                    Some(Value::Num(n)) if *n != 0.0 && n.is_finite() => *n,
                    _ => 0.0,
                },
                Value::Num(n) if i == 0 => *n,
                _ => 0.0,
            }
        };
        match pt {
            Value::Arr(_) | Value::Num(_) => Some((get(pt, 0), get(pt, 1))),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Operators
// ---------------------------------------------------------------------------

fn binary(op: ast::BinaryOperator, l: &Value, r: &Value) -> Option<Value> {
    use ast::BinaryOperator as B;
    match op {
        B::Equality | B::StrictEquality => return Some(Value::Bool(loose_eq(l, r)?)),
        B::Inequality | B::StrictInequality => return Some(Value::Bool(!loose_eq(l, r)?)),
        _ => {}
    }
    if op == B::Addition && (matches!(l, Value::Str(_)) || matches!(r, Value::Str(_))) {
        // Only the both-strings case; mixed coercion has number formatting
        // rules this interpreter has no reason to reproduce.
        return match (l, r) {
            (Value::Str(a), Value::Str(b)) => Some(Value::Str(format!("{a}{b}"))),
            _ => None,
        };
    }
    let (a, b) = (l.num()?, r.num()?);
    Some(match op {
        B::Addition => Value::Num(a + b),
        B::Subtraction => Value::Num(a - b),
        B::Multiplication => Value::Num(a * b),
        B::Division => Value::Num(a / b),
        // Rust's `%` on floats is the C remainder, which is JS's `%`.
        B::Remainder => Value::Num(a % b),
        B::Exponential => Value::Num(a.powf(b)),
        B::LessThan => Value::Bool(a < b),
        B::LessEqualThan => Value::Bool(a <= b),
        B::GreaterThan => Value::Bool(a > b),
        B::GreaterEqualThan => Value::Bool(a >= b),
        _ => return None,
    })
}

/// JS equality over the shapes this interpreter holds, `None` where the
/// coercion rules are anything but obvious.
fn loose_eq(l: &Value, r: &Value) -> Option<bool> {
    let nullish = |v: &Value| matches!(v, Value::Null | Value::Undefined);
    if nullish(l) || nullish(r) {
        return Some(nullish(l) && nullish(r));
    }
    match (l, r) {
        (Value::Str(a), Value::Str(b)) => Some(a == b),
        // Numbers and booleans compare numerically (`1 == true`).
        _ if l.num().is_some() && r.num().is_some() => Some(l.num()? == r.num()?),
        // A record index is a layer's identity.
        (Value::Layer(a), Value::Layer(b)) => Some(a == b),
        // Arrays and paths compare by reference in JS; this interpreter has
        // no references, so it declines rather than guess.
        _ => None,
    }
}

fn math(name: &str, args: &[Value]) -> Option<Value> {
    let a = args.first().and_then(Value::num);
    Some(Value::Num(match name {
        "abs" => a?.abs(),
        "floor" => a?.floor(),
        "ceil" => a?.ceil(),
        "round" => a?.round(),
        "sqrt" => a?.sqrt(),
        "sin" => a?.sin(),
        "cos" => a?.cos(),
        "tan" => a?.tan(),
        "atan" => a?.atan(),
        "exp" => a?.exp(),
        "log" => a?.ln(),
        "atan2" => a?.atan2(args.get(1)?.num()?),
        "pow" => a?.powf(args.get(1)?.num()?),
        "min" => a?.min(args.get(1)?.num()?),
        "max" => a?.max(args.get(1)?.num()?),
        "hypot" => args
            .iter()
            .map(|v| v.num())
            .collect::<Option<Vec<_>>>()?
            .into_iter()
            .fold(0.0, f64::hypot),
        _ => return None,
    }))
}

/// `sum` / `sub` / `mul` / `div` over two arguments: scalar↔array broadcast,
/// the shorter array padded with the operator's unit (`zip` in expr.js).
fn zip(args: &[Value], unit: f64, op: impl Fn(f64, f64) -> f64) -> Option<Value> {
    let (a, b) = (args.first()?, args.get(1)?);
    match (a, b) {
        (Value::Arr(x), Value::Arr(y)) => Some(Value::Arr(
            x.iter()
                .enumerate()
                .map(|(i, x)| {
                    let y = y.get(i).and_then(Value::num).unwrap_or(unit);
                    Some(Value::Num(op(x.num()?, y)))
                })
                .collect::<Option<Vec<_>>>()?,
        )),
        (Value::Arr(x), b) => Some(Value::Arr(
            x.iter()
                .map(|x| Some(Value::Num(op(x.num()?, b.num()?))))
                .collect::<Option<Vec<_>>>()?,
        )),
        (a, Value::Arr(y)) => Some(Value::Arr(
            y.iter()
                .map(|y| Some(Value::Num(op(a.num()?, y.num()?))))
                .collect::<Option<Vec<_>>>()?,
        )),
        (a, b) => Some(Value::Num(op(a.num()?, b.num()?))),
    }
}

// ---------------------------------------------------------------------------
// Keyframes and paths — the `keyedProperty` / `pathProperty` data shapes
// ---------------------------------------------------------------------------

fn key_value(a: &Anim, i: usize) -> Value {
    match a.kind {
        AnimKind::Path => match a.paths.get(i) {
            Some(p) => Value::Path(PathVal {
                v: p.v.clone(),
                i: p.i.clone(),
                o: p.o.clone(),
                c: p.c,
            }),
            None => Value::Undefined,
        },
        // `keyValue`: a scalar column, or a dim of 1, indexes directly.
        AnimKind::Scalar => Value::Num(a.v.get(i).copied().unwrap_or(0.0)),
        AnimKind::Vector => Value::Arr(
            (0..a.dim)
                .map(|k| Value::Num(a.v.get(i * a.dim + k).copied().unwrap_or(0.0)))
                .collect(),
        ),
    }
}

/// Flat `[x, y, …]` → `[[x, y], …]`, the shape the path accessors return.
fn pairs(path: &PathVal, key: Option<&str>) -> Value {
    let src = match key {
        Some("i") => &path.i,
        Some("o") => &path.o,
        _ => &path.v,
    };
    if src.is_empty() {
        // `pairs`' absent-tangent branch: zeros, one pair per vertex.
        let n = path.v.len() >> 1;
        return Value::Arr(
            (0..n)
                .map(|_| Value::Arr(vec![Value::Num(0.0), Value::Num(0.0)]))
                .collect(),
        );
    }
    Value::Arr(
        src.chunks(2)
            .map(|c| {
                Value::Arr(vec![
                    Value::Num(c.first().copied().unwrap_or(0.0)),
                    Value::Num(c.get(1).copied().unwrap_or(0.0)),
                ])
            })
            .collect(),
    )
}

fn create_path(
    verts: Option<&Value>,
    in_tan: Option<&Value>,
    out_tan: Option<&Value>,
    closed: Option<&Value>,
) -> Option<PathVal> {
    let flat = |src: Option<&Value>| -> Option<Vec<f64>> {
        let mut out = Vec::new();
        if let Some(Value::Arr(items)) = src {
            for p in items {
                let Value::Arr(pair) = p else { return None };
                if pair.len() != 2 {
                    return None;
                }
                out.push(pair[0].num()?);
                out.push(pair[1].num()?);
            }
        }
        Some(out)
    };
    Some(PathVal {
        v: flat(verts)?,
        i: flat(in_tan)?,
        o: flat(out_tan)?,
        c: closed.is_some_and(|v| v.truthy()),
    })
}

// ---------------------------------------------------------------------------
// Arc-length sampling — `arcTable` / `locate` / `pointOnPath` / `tangentOnPath`
// ---------------------------------------------------------------------------

/// Arc-length samples per segment. Matches `ARC` in expr.js.
const ARC: usize = 300;

struct Seg {
    samples: Vec<f64>,
    len: f64,
    /// `[p0x, p0y, p1x, p1y, p2x, p2y, p3x, p3y]` — the bezier control points.
    p: [f64; 8],
}

fn arc_table(path: &PathVal) -> Option<(Vec<Seg>, f64)> {
    let v = &path.v;
    let n = v.len() >> 1;
    if n == 0 {
        return Some((Vec::new(), 0.0));
    }
    let segs = if path.c { n } else { n - 1 };
    let mut cumul = Vec::with_capacity(segs);
    let mut total = 0.0;
    for s in 0..segs {
        let a = s * 2;
        let b = ((s + 1) % n) * 2;
        let (p0x, p0y) = (v[a], v[a + 1]);
        let (p3x, p3y) = (v[b], v[b + 1]);
        let (p1x, p1y) = (
            p0x + path.o.get(a).copied().unwrap_or(0.0),
            p0y + path.o.get(a + 1).copied().unwrap_or(0.0),
        );
        let (p2x, p2y) = (
            p3x + path.i.get(b).copied().unwrap_or(0.0),
            p3y + path.i.get(b + 1).copied().unwrap_or(0.0),
        );
        let mut samples = vec![0.0; ARC + 1];
        let (mut acc, mut px, mut py) = (0.0, p0x, p0y);
        // `samples[k]` accumulates as `k` advances; the index is the point.
        #[allow(clippy::needless_range_loop)]
        for k in 1..=ARC {
            let t = k as f64 / ARC as f64;
            let u = 1.0 - t;
            let (u3, u2t, ut2, t3) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
            let x = u3 * p0x + u2t * p1x + ut2 * p2x + t3 * p3x;
            let y = u3 * p0y + u2t * p1y + ut2 * p2y + t3 * p3y;
            acc += (x - px).hypot(y - py);
            samples[k] = acc;
            px = x;
            py = y;
        }
        total += acc;
        cumul.push(Seg {
            samples,
            len: acc,
            p: [p0x, p0y, p1x, p1y, p2x, p2y, p3x, p3y],
        });
    }
    Some((cumul, total))
}

/// Locate `(control points, t)` at arc-length fraction `u`.
fn locate(path: &PathVal, u: f64) -> Option<([f64; 8], f64)> {
    let (cumul, total) = arc_table(path)?;
    if cumul.is_empty() || total == 0.0 {
        return None;
    }
    let target = u.clamp(0.0, 1.0) * total;
    let mut acc = 0.0;
    for (s, seg) in cumul.iter().enumerate() {
        if target <= acc + seg.len || s == cumul.len() - 1 {
            let local = target - acc;
            let (mut lo, mut hi) = (0usize, ARC);
            while lo < hi {
                let m = (lo + hi) >> 1;
                if seg.samples[m] < local {
                    lo = m + 1;
                } else {
                    hi = m;
                }
            }
            let (up, low) = (lo, lo.saturating_sub(1));
            let (dl, dh) = (seg.samples[low], seg.samples[up]);
            let f = if dh == dl {
                0.0
            } else {
                (local - dl) / (dh - dl)
            };
            return Some((seg.p, ((low as f64 + f) / ARC as f64).clamp(0.0, 1.0)));
        }
        acc += seg.len;
    }
    None
}

fn point_on_path(path: Option<&PathVal>, u: f64) -> Value {
    let Some(path) = path else {
        return Value::Arr(vec![Value::Num(0.0), Value::Num(0.0)]);
    };
    let Some((p, t)) = locate(path, u) else {
        return Value::Arr(vec![
            Value::Num(js_or0(path.v.first().copied())),
            Value::Num(js_or0(path.v.get(1).copied())),
        ]);
    };
    let m = 1.0 - t;
    let (u3, u2t, ut2, t3) = (m * m * m, 3.0 * m * m * t, 3.0 * m * t * t, t * t * t);
    Value::Arr(vec![
        Value::Num(u3 * p[0] + u2t * p[2] + ut2 * p[4] + t3 * p[6]),
        Value::Num(u3 * p[1] + u2t * p[3] + ut2 * p[5] + t3 * p[7]),
    ])
}

fn tangent_on_path(path: Option<&PathVal>, u: f64) -> Value {
    let Some(path) = path else {
        return Value::Arr(vec![Value::Num(1.0), Value::Num(0.0)]);
    };
    let Some((p, t)) = locate(path, u) else {
        return Value::Arr(vec![Value::Num(1.0), Value::Num(0.0)]);
    };
    let m = 1.0 - t;
    Value::Arr(vec![
        Value::Num(
            3.0 * m * m * (p[2] - p[0]) + 6.0 * m * t * (p[4] - p[2]) + 3.0 * t * t * (p[6] - p[4]),
        ),
        Value::Num(
            3.0 * m * m * (p[3] - p[1]) + 6.0 * m * t * (p[5] - p[3]) + 3.0 * t * t * (p[7] - p[5]),
        ),
    ])
}

/// JS `x || 0`: 0, NaN and missing all read as 0.
fn js_or0(v: Option<f64>) -> f64 {
    match v {
        Some(x) if x.is_finite() && x != 0.0 => x,
        _ => 0.0,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// `expression_layer_ref`, patched: the smallest fixture whose expressions
    /// already compile, with the body and the inputs swapped per case.
    fn doc(patch: impl Fn(&mut serde_json::Value)) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../_fixtures/animations/expression_layer_ref.json");
        let mut d: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        patch(&mut d);
        crate::compile_document(&serde_json::to_string(&d).unwrap()).unwrap()
    }

    /// The Follower layer (index 0), whose transform the test bodies drive.
    fn follower(d: &mut serde_json::Value) -> &mut serde_json::Value {
        &mut d["layers"][0]["ks"]
    }

    /// The Fader layer (index 1), the usual reference target.
    fn fader(d: &mut serde_json::Value) -> &mut serde_json::Value {
        &mut d["layers"][1]["ks"]
    }

    #[test]
    fn a_cross_layer_read_bakes_what_the_body_computes() {
        // `sum(thisComp.layer('Fader').transform.position, [10, 20])` with the
        // Fader parked at (30, 40) — not the property's own fallback (60, 100).
        let doc = doc(|d| {
            fader(d)["p"] = serde_json::json!({"a": 0, "k": [30, 40, 0]});
            follower(d)["p"]["x"] = serde_json::json!(
                "var $bm_rt;\n$bm_rt = sum(thisComp.layer('Fader').transform.position, [10, 20]);"
            );
        });
        assert!(
            doc.contains("matrix(1,0,0,1,40,60)"),
            "follower did not bake to the expression's (40, 60):\n{doc}"
        );
        assert!(
            !doc.contains("matrix(1,0,0,1,60,100)"),
            "follower baked to its fallback instead"
        );
    }

    #[test]
    fn a_construct_the_interpreter_cannot_decide_bakes_the_fallback() {
        let doc = doc(|d| {
            follower(d)["p"]["x"] = serde_json::json!("var $bm_rt;\n$bm_rt = noSuchHelper(value);");
        });
        assert!(
            doc.contains("matrix(1,0,0,1,60,100)"),
            "an undecidable body must fall back to the property's own value:\n{doc}"
        );
    }

    #[test]
    fn a_keyed_surface_reads_the_frame_it_is_shown() {
        // The property's own keyframes run 100 → 0 across frames 0..30, and
        // the body asks for t = 0.5 s = frame 15: the midpoint, 50.
        let doc = doc(|d| {
            follower(d)["o"] = serde_json::json!({
                "a": 1,
                "k": [
                    {"t": 0, "s": [100], "i": {"x": [0.5], "y": [1]}, "o": {"x": [0.5], "y": [0]}},
                    {"t": 30, "s": [0]}
                ],
                "x": "var $bm_rt;\n$bm_rt = thisProperty.valueAtTime(0.5);"
            });
        });
        assert!(
            doc.contains("opacity=\"0.5\""),
            "valueAtTime(0.5) should bake opacity 50:\n{doc}"
        );
    }

    #[test]
    fn a_drill_chain_point_on_path_and_to_comp_agree_by_hand() {
        // The Fader's rectangle becomes an explicit square, 40 × 40 centred on
        // its own origin, and the layer moves to (100, 90). A square's
        // arc-length is its perimeter, so u = 0.125 is exactly the midpoint of
        // the first edge — (20, 0) — and toComp lands on (120, 90).
        let doc = doc(|d| {
            fader(d)["p"] = serde_json::json!({"a": 0, "k": [100, 90, 0]});
            let sh = serde_json::json!({
                "ty": "sh", "nm": "Path 1",
                "ks": {"a": 0, "k": {
                    "i": [[0, 0], [0, 0], [0, 0], [0, 0]],
                    "o": [[0, 0], [0, 0], [0, 0], [0, 0]],
                    "v": [[20, -20], [20, 20], [-20, 20], [-20, -20]],
                    "c": true
                }}
            });
            d["layers"][1]["shapes"][0]["it"][0] = sh;
            follower(d)["p"]["x"] = serde_json::json!(
                "var $bm_rt;\n\
                 var wire = thisComp.layer('Fader');\n\
                 var p = wire('ADBE Root Vectors Group')(1)('ADBE Vectors Group')(1)('ADBE Vector Shape');\n\
                 $bm_rt = wire.toComp(p.pointOnPath(0.125));"
            );
        });
        assert!(
            doc.contains("matrix(1,0,0,1,120,90)"),
            "pointOnPath(0.125) + toComp should bake to (120, 90):\n{doc}"
        );
    }

    #[test]
    fn bare_loop_out_on_a_property_inside_its_range_reads_the_current_frame() {
        // Frame 0 is at or before the last key, so loopOut is the property's
        // own value there — 100 — which is also what the fallback bakes. The
        // assertion is that the body evaluates at all rather than refusing.
        let doc = doc(|d| {
            follower(d)["o"] = serde_json::json!({
                "a": 1,
                "k": [
                    {"t": 0, "s": [100], "i": {"x": [0.5], "y": [1]}, "o": {"x": [0.5], "y": [0]}},
                    {"t": 30, "s": [0]}
                ],
                "x": "var $bm_rt;\n$bm_rt = loopOut('cycle');"
            });
        });
        assert!(doc.contains("opacity=\"1\""), "{doc}");
    }

    #[test]
    fn the_interpreter_refuses_bodies_with_no_owning_layer() {
        // A body reaching for `thisLayer` on a property with no owner cannot
        // resolve anything; the bake must fall back rather than guess 0.
        let out = eval_at(
            "var $bm_rt;\n$bm_rt = thisLayer.index;",
            &Site {
                frame: 0.0,
                fr: 30.0,
                owner: None,
                fallback: None,
                layers: &[],
                scopes: &[],
                names: &[],
            },
            &NoHost,
        );
        assert_eq!(out, None);
    }

    struct NoHost;
    impl Host for NoHost {
        fn prop_at(&self, _: &Prop, _: f64) -> Option<Value> {
            None
        }
    }
}
