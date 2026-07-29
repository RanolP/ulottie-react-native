//! Expression compilation — resolve what the compiler already knows, fold what
//! that makes constant, and delete what turns out to do nothing.
//!
//! An After Effects expression reaches its inputs by *name*, at runtime:
//! `thisComp.layer('wire')`, `effect('Position - Overshoot')('ADBE Slider
//! Control-0001')`, `thisProperty.propertyGroup(1)('…-0002')`. Every one of
//! those names is something the compiler resolved long ago. Shipping them means
//! shipping three things: the names themselves, a body that looks them up, and
//! the machinery that makes the lookup work — of which `proxyFor` is the single
//! largest function in the runtime.
//!
//! So this stage reads the body as syntax and evaluates it against what is
//! known. Three verdicts:
//!
//! * [`Outcome::Identity`] — the body returns the property's own value. The
//!   expression is deleted and the property is its keyframes. This is not a
//!   rare case: Bodymovin emits `if (<some effect checkbox>) { … } else {
//!   $bm_rt = value; }` for every property a "loop" toggle can reach, whether
//!   or not the toggle is on.
//! * [`Outcome::Constant`] — the body is a compile-time constant, so the
//!   property is that constant and the whole animated path goes away with it.
//! * [`Outcome::Open`] — nothing could be decided. The expression ships.
//!
//! Everything here is conservative in one direction: an input that cannot be
//! resolved *exactly* yields `Open`, never a guess. A wrong fold is a silent
//! rendering change, and the only thing a missed fold costs is bytes.
//!
//! What this stage deliberately does **not** do is rewrite a surviving body.
//! Resolving the references inside one — turning `layer('wire')(…)(1)(…)` into
//! a direct handle so the proxy can go — is the same analysis pointed at a
//! different output, and wants the emitter to have somewhere to put a handle.
//! It has one: [`crate::backend::layers`], which runs per planned scene because
//! a record index only means anything against the table the planner built.
//! [`resolve`] stays here rather than moving there because it needs no scene —
//! it only ever resolves the *owning* layer's own effects, which the IR knows.

use std::collections::HashMap;

use oxc_allocator::Allocator;
use oxc_ast::ast;
use oxc_parser::Parser;
use oxc_span::SourceType;

use crate::ir;

mod pass;
pub mod resolve;
pub use pass::fold_module;

/// What the compiler decided about one expression, at one property.
///
/// Per *property*, not per expression: the bodies are deduplicated, so the same
/// body appears on many properties and folds differently on each. `lights`
/// carries one loop-toggle expression on five layers.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Returns `value` unchanged — delete it.
    Identity,
    /// A compile-time constant.
    Constant(f64),
    /// Undecidable. Ship the expression.
    Open,
}

/// What is known about the property an expression is attached to.
pub struct Facts<'a> {
    /// Effects on the layer that owns the property. Expressions reach these by
    /// name, both as `effect('…')` and through `propertyGroup`.
    pub effects: &'a [ir::Effect],
    /// Keyframes on the property itself, which `numKeys` reports.
    pub num_keys: usize,
    /// Range of the property's own value across its keyframes, when numeric.
    /// `clamp(value, 0, 100)` is the identity on a property that never leaves
    /// `0..=100`, and Bodymovin emits that clamp whether or not it can.
    pub value_range: Option<(f64, f64)>,
}

/// Evaluate `body` against `facts`.
pub fn fold(body: &str, facts: &Facts) -> Outcome {
    let allocator = Allocator::default();
    // The bodies are function bodies, not modules, but every construct they use
    // parses the same either way and nothing here looks at module semantics.
    let parsed = Parser::new(&allocator, body, SourceType::cjs()).parse();
    if !parsed.errors.is_empty() {
        return Outcome::Open;
    }

    let mut env = Env { vars: HashMap::new(), facts };
    for stmt in &parsed.program.body {
        match env.exec(stmt) {
            Some(Flow::Normal) => {}
            Some(Flow::Return(v)) => return verdict(Some(v)),
            None => return Outcome::Open,
        }
    }
    // Bodymovin assigns the result to `$bm_rt` and the emitter returns it.
    verdict(env.vars.get("$bm_rt").cloned())
}

fn verdict(v: Option<V>) -> Outcome {
    match v {
        Some(V::Value) => Outcome::Identity,
        Some(V::Num(n)) if n.is_finite() => Outcome::Constant(n),
        Some(V::Bool(b)) => Outcome::Constant(if b { 1.0 } else { 0.0 }),
        _ => Outcome::Open,
    }
}

// ---------------------------------------------------------------------------
// Values
// ---------------------------------------------------------------------------

/// A value carried at compile time.
///
/// [`V::Value`] is the interesting one: the property's own value, held
/// symbolically rather than evaluated. An expression that returns it is an
/// expression that does nothing, which is the whole point of the pass.
#[derive(Debug, Clone, PartialEq)]
enum V {
    Num(f64),
    Bool(bool),
    Str(String),
    /// `value` / `thisProperty`, unevaluated.
    Value,
    /// `effect('name')`, before a parameter is selected off it.
    Effect(usize),
    /// `thisProperty.propertyGroup(n)` — the group holding this property.
    /// Selecting off it searches the layer's effects, and refuses to guess when
    /// more than one could match.
    Group,
}

impl V {
    fn num(&self) -> Option<f64> {
        match self {
            V::Num(n) => Some(*n),
            V::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            _ => None,
        }
    }

    /// JavaScript truthiness, for the values that have one here.
    fn truthy(&self) -> Option<bool> {
        match self {
            V::Bool(b) => Some(*b),
            V::Num(n) => Some(*n != 0.0 && !n.is_nan()),
            V::Str(s) => Some(!s.is_empty()),
            _ => None,
        }
    }
}

enum Flow {
    Normal,
    Return(V),
}

struct Env<'a> {
    vars: HashMap<String, V>,
    facts: &'a Facts<'a>,
}

// ---------------------------------------------------------------------------
// Statements
// ---------------------------------------------------------------------------

impl Env<'_> {
    /// `None` means "cannot decide" and aborts the whole fold.
    fn exec(&mut self, stmt: &ast::Statement) -> Option<Flow> {
        match stmt {
            ast::Statement::VariableDeclaration(d) => {
                for decl in &d.declarations {
                    let name = decl.id.get_binding_identifier()?.name.as_str().to_string();
                    match &decl.init {
                        // `var x;` with no initializer is a declaration only.
                        None => {}
                        Some(e) => {
                            let v = self.eval(e)?;
                            self.vars.insert(name, v);
                        }
                    }
                }
                Some(Flow::Normal)
            }
            ast::Statement::ExpressionStatement(s) => {
                self.eval(&s.expression)?;
                Some(Flow::Normal)
            }
            ast::Statement::BlockStatement(b) => {
                for s in &b.body {
                    match self.exec(s)? {
                        Flow::Normal => {}
                        r => return Some(r),
                    }
                }
                Some(Flow::Normal)
            }
            // The branch is only taken when the test is decidable. A test that
            // depends on `value` leaves the whole expression open.
            ast::Statement::IfStatement(s) => {
                let taken = self.eval(&s.test)?.truthy()?;
                if taken {
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
                None => None,
            },
            ast::Statement::EmptyStatement(_) => Some(Flow::Normal),
            // A `try` whose body folds cleanly cannot throw, so the catch is
            // unreachable and can be dropped. If it does not fold, the whole
            // expression is open — the catch is not a licence to guess.
            ast::Statement::TryStatement(s) => {
                for st in &s.block.body {
                    match self.exec(st)? {
                        Flow::Normal => {}
                        r => return Some(r),
                    }
                }
                Some(Flow::Normal)
            }
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Expressions
// ---------------------------------------------------------------------------

impl Env<'_> {
    fn eval(&mut self, e: &ast::Expression) -> Option<V> {
        match e {
            ast::Expression::NumericLiteral(n) => Some(V::Num(n.value)),
            ast::Expression::BooleanLiteral(b) => Some(V::Bool(b.value)),
            ast::Expression::StringLiteral(s) => Some(V::Str(s.value.as_str().to_string())),
            ast::Expression::Identifier(id) => self.ident(id.name.as_str()),
            ast::Expression::ParenthesizedExpression(p) => self.eval(&p.expression),
            ast::Expression::SequenceExpression(s) => {
                let mut last = None;
                for e in &s.expressions {
                    last = Some(self.eval(e)?);
                }
                last
            }
            ast::Expression::AssignmentExpression(a) => {
                let v = self.eval(&a.right)?;
                // Only plain `x = …`; a compound assignment would need the old
                // value and none of these bodies use one.
                if !matches!(a.operator, ast::AssignmentOperator::Assign) {
                    return None;
                }
                let name = a.left.get_identifier_name()?;
                self.vars.insert(name.to_string(), v.clone());
                Some(v)
            }
            ast::Expression::UnaryExpression(u) => {
                let v = self.eval(&u.argument)?;
                match u.operator {
                    ast::UnaryOperator::UnaryNegation => Some(V::Num(-v.num()?)),
                    ast::UnaryOperator::UnaryPlus => Some(V::Num(v.num()?)),
                    ast::UnaryOperator::LogicalNot => Some(V::Bool(!v.truthy()?)),
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
                    // Short-circuit on the left when it decides the result, so
                    // `<false> && <anything>` folds without the right side
                    // having to be foldable at all.
                    ast::LogicalOperator::And => {
                        if !left.truthy()? {
                            Some(left)
                        } else {
                            self.eval(&l.right)
                        }
                    }
                    ast::LogicalOperator::Or => {
                        if left.truthy()? {
                            Some(left)
                        } else {
                            self.eval(&l.right)
                        }
                    }
                    ast::LogicalOperator::Coalesce => None,
                }
            }
            ast::Expression::ConditionalExpression(c) => {
                if self.eval(&c.test)?.truthy()? {
                    self.eval(&c.consequent)
                } else {
                    self.eval(&c.alternate)
                }
            }
            ast::Expression::StaticMemberExpression(m) => {
                let obj = self.eval(&m.object)?;
                self.member(&obj, m.property.name.as_str())
            }
            ast::Expression::CallExpression(c) => self.call(c),
            _ => None,
        }
    }

    fn ident(&self, name: &str) -> Option<V> {
        if let Some(v) = self.vars.get(name) {
            return Some(v.clone());
        }
        match name {
            "value" | "thisProperty" => Some(V::Value),
            // The emitter binds this off `thisProperty`, so it is the same
            // constant whether the body reads it free or through the property.
            "numKeys" => Some(V::Num(self.facts.num_keys as f64)),
            "true" => Some(V::Bool(true)),
            "false" => Some(V::Bool(false)),
            _ => None,
        }
    }

    fn member(&self, obj: &V, prop: &str) -> Option<V> {
        match (obj, prop) {
            (V::Value, "numKeys") => Some(V::Num(self.facts.num_keys as f64)),
            // `Math` is a namespace, not a value; the call path handles it.
            _ => None,
        }
    }

    fn call(&mut self, c: &ast::CallExpression) -> Option<V> {
        // `Math.*` and the Bodymovin arithmetic helpers, which are plain
        // functions over numbers.
        if let ast::Expression::StaticMemberExpression(m) = &c.callee {
            if let ast::Expression::Identifier(id) = &m.object {
                if id.name == "Math" {
                    let args = self.args(c)?;
                    return math(m.property.name.as_str(), &args);
                }
            }
            // `thisProperty.propertyGroup(n)` — the group holding it.
            let name = m.property.name.as_str();
            let obj = self.eval(&m.object)?;
            if obj == V::Value && name == "propertyGroup" {
                return Some(V::Group);
            }
            // `thisLayer.effect('name')` and, on `thisProperty`, nothing else
            // is decidable: `loopOut`, `valueAtTime` and friends all depend on
            // the frame, which this pass does not have.
            if name == "effect" {
                let args = self.args(c)?;
                return self.effect(args.first()?);
            }
            return None;
        }

        if let ast::Expression::Identifier(id) = &c.callee {
            let args = self.args(c)?;
            return match id.name.as_str() {
                "effect" => self.effect(args.first()?),
                "clamp" => self.clamp(&args),
                "sum" | "add" => arith(&args, |a, b| a + b),
                "sub" => arith(&args, |a, b| a - b),
                "mul" => arith(&args, |a, b| a * b),
                "div" => arith(&args, |a, b| a / b),
                "radiansToDegrees" => Some(V::Num(args.first()?.num()?.to_degrees())),
                "degreesToRadians" => Some(V::Num(args.first()?.num()?.to_radians())),
                _ => None,
            };
        }

        // A call on the result of another call: `effect('n')('param')` and
        // `propertyGroup(1)('param')`, which is how AE spells member access.
        let callee = self.eval_callee(c)?;
        let args = self.args(c)?;
        match callee {
            V::Effect(i) => {
                let key = match args.first()? {
                    V::Str(s) => s.clone(),
                    _ => return None,
                };
                param_value(&self.facts.effects[i], &key)
            }
            V::Group => {
                let key = match args.first()? {
                    V::Str(s) => s.clone(),
                    _ => return None,
                };
                self.sibling(&key)
            }
            _ => None,
        }
    }

    fn eval_callee(&mut self, c: &ast::CallExpression) -> Option<V> {
        match &c.callee {
            ast::Expression::CallExpression(inner) => self.call(inner),
            ast::Expression::Identifier(id) => self.ident(id.name.as_str()),
            ast::Expression::ParenthesizedExpression(p) => self.eval(&p.expression),
            _ => None,
        }
    }

    fn args(&mut self, c: &ast::CallExpression) -> Option<Vec<V>> {
        let mut out = Vec::with_capacity(c.arguments.len());
        for a in &c.arguments {
            out.push(self.eval(a.as_expression()?)?);
        }
        Some(out)
    }

    /// `effect('name')` on the owning layer, by display name or match name.
    fn effect(&self, key: &V) -> Option<V> {
        let V::Str(key) = key else { return None };
        let mut found = None;
        for (i, e) in self.facts.effects.iter().enumerate() {
            if e.name.as_deref() == Some(key.as_str())
                || e.match_name.as_deref() == Some(key.as_str())
            {
                // Two effects answering to one name: refuse rather than pick.
                if found.is_some() {
                    return None;
                }
                found = Some(i);
            }
        }
        found.map(V::Effect)
    }

    /// A parameter reached through `propertyGroup`, which names a sibling of
    /// the property this expression is on.
    ///
    /// Which effect that group *is* is not tracked, so this searches every
    /// effect on the layer and refuses when more than one has the name. Pseudo
    /// effect parameters carry match names like `Pseudo/ADBE Trace Path-0002`,
    /// which collide only between two instances of the same pseudo effect —
    /// exactly the case the ambiguity check declines.
    fn sibling(&self, key: &str) -> Option<V> {
        let mut found = None;
        for e in self.facts.effects {
            if let Some(v) = param_value(e, key) {
                if found.is_some() {
                    return None;
                }
                found = Some(v);
            }
        }
        found
    }

    /// `clamp(v, lo, hi)`.
    ///
    /// The interesting case is the symbolic one: clamping a property to a range
    /// it never leaves is the identity, and Bodymovin emits `clamp(value, 0,
    /// 100)` on opacity whether or not the keyframes need it.
    fn clamp(&self, args: &[V]) -> Option<V> {
        let (v, lo, hi) = (args.first()?, args.get(1)?.num()?, args.get(2)?.num()?);
        if let Some(n) = v.num() {
            return Some(V::Num(n.clamp(lo, hi)));
        }
        if *v == V::Value {
            let (min, max) = self.facts.value_range?;
            if min >= lo && max <= hi {
                return Some(V::Value);
            }
        }
        None
    }
}

// ---------------------------------------------------------------------------
// Operators and helpers
// ---------------------------------------------------------------------------

fn binary(op: ast::BinaryOperator, l: &V, r: &V) -> Option<V> {
    use ast::BinaryOperator as B;
    // Equality is decidable between two known values of the same shape; AE
    // bodies compare an effect checkbox against `true`, which in the payload is
    // the number 1.
    match op {
        B::Equality | B::StrictEquality => {
            let eq = match (l, r) {
                (V::Str(a), V::Str(b)) => a == b,
                _ => l.num()? == r.num()?,
            };
            return Some(V::Bool(eq));
        }
        B::Inequality | B::StrictInequality => {
            let eq = match (l, r) {
                (V::Str(a), V::Str(b)) => a == b,
                _ => l.num()? == r.num()?,
            };
            return Some(V::Bool(!eq));
        }
        _ => {}
    }
    let (a, b) = (l.num()?, r.num()?);
    Some(match op {
        B::Addition => V::Num(a + b),
        B::Subtraction => V::Num(a - b),
        B::Multiplication => V::Num(a * b),
        B::Division => V::Num(a / b),
        B::Remainder => V::Num(a % b),
        B::Exponential => V::Num(a.powf(b)),
        B::LessThan => V::Bool(a < b),
        B::LessEqualThan => V::Bool(a <= b),
        B::GreaterThan => V::Bool(a > b),
        B::GreaterEqualThan => V::Bool(a >= b),
        _ => return None,
    })
}

fn arith(args: &[V], f: impl Fn(f64, f64) -> f64) -> Option<V> {
    Some(V::Num(f(args.first()?.num()?, args.get(1)?.num()?)))
}

fn math(name: &str, args: &[V]) -> Option<V> {
    let a = args.first().and_then(V::num);
    Some(V::Num(match name {
        "abs" => a?.abs(),
        "floor" => a?.floor(),
        "ceil" => a?.ceil(),
        "round" => a?.round(),
        "sqrt" => a?.sqrt(),
        "sin" => a?.sin(),
        "cos" => a?.cos(),
        "exp" => a?.exp(),
        "atan2" => a?.atan2(args.get(1)?.num()?),
        "pow" => a?.powf(args.get(1)?.num()?),
        "min" => a?.min(args.get(1)?.num()?),
        "max" => a?.max(args.get(1)?.num()?),
        _ => return None,
    }))
}

/// One effect parameter's value, when it is a compile-time constant.
///
/// An animated parameter has no single value and yields `None` — the property
/// it drives stays an expression.
fn param_value(effect: &ir::Effect, key: &str) -> Option<V> {
    for p in &effect.parameters {
        if p.name.as_deref() == Some(key) || p.match_name.as_deref() == Some(key) {
            let ir::EffectValue::Scalar(prop) = &p.value else { return None };
            return prop.static_value().copied().map(V::Num);
        }
    }
    None
}

#[cfg(test)]
mod tests;
