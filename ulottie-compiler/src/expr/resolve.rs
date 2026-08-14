//! Reference resolution: turn `effect('Position - Overshoot')('ADBE Slider
//! Control-0001')` into `effect(2)(0)`.
//!
//! This is where the bytes are. Almost nothing in an expression string table is
//! a layer name — `lights` carries 454 B of names of which four are — the rest
//! is After Effects effect and parameter names, shipped so that a body can look
//! at runtime for something the compiler resolved at build time. Each one is
//! paid for three times over: the literal in the body, the name on the effect
//! in the payload, and the linear search in `proxyFor` that matches them up.
//!
//! Only the *owning* layer's effects are resolved, which is what a bare
//! `effect(…)` and `thisLayer.effect(…)` both mean. `thisComp.layer('x')
//! .effect(…)` reaches a different layer and is left alone.
//!
//! Bodies are deduplicated, so one is shared by every layer it was applied to
//! and a rewrite has to be right for all of them. The indices are resolved once
//! per using layer and the rewrite only happens if they all agree — which they
//! do whenever the same effect was applied to a set of sibling layers, and
//! that is the case worth having.

use oxc_allocator::Allocator;
use oxc_ast::ast;
use oxc_parser::Parser;
use oxc_span::{GetSpan, SourceType};

use crate::ir;

/// A group of name literals the compiler can replace with indices.
///
/// Both forms resolve to a flat list of numbers parallel to [`Ref::spans`], so
/// the agreement check across a shared body's uses is a list comparison and the
/// rewrite is a splice.
pub enum Ref {
    /// `effect('name')('param')` — the literals are right there.
    Direct {
        name: Span,
        param: Span,
        name_str: String,
        param_str: String,
    },
    /// `var names = ['a', 'b', …]` indexed into `effect(names[i])('param')`.
    ///
    /// After Effects generates this for anything applied across a set of
    /// layers: the names are constants, the loop bound is `names.length`, and
    /// only the index varies. Replacing the elements with effect indices makes
    /// the lookup positional without touching the loop.
    Table {
        elems: Vec<(Span, String)>,
        param: Span,
        param_str: String,
    },
}

#[derive(Clone, Copy, PartialEq)]
pub struct Span {
    start: usize,
    end: usize,
}

impl Ref {
    /// The literals this reference would replace, in body order.
    fn spans(&self) -> Vec<Span> {
        match self {
            Ref::Direct { name, param, .. } => vec![*name, *param],
            Ref::Table { elems, param, .. } => {
                elems.iter().map(|(s, _)| *s).chain([*param]).collect()
            }
        }
    }

    /// Indices for each span, against one layer's effects.
    ///
    /// `None` whenever anything is ambiguous or absent — two effects answering
    /// to one name, a parameter that is not on the effect, or a table whose
    /// effects disagree about where the parameter sits.
    pub fn resolve(&self, effects: &[ir::Effect]) -> Option<Vec<u32>> {
        match self {
            Ref::Direct {
                name_str,
                param_str,
                ..
            } => {
                let e = effect_index(effects, name_str)?;
                let p = param_index(&effects[e as usize], param_str)?;
                Some(vec![e, p])
            }
            Ref::Table {
                elems, param_str, ..
            } => {
                let mut out = Vec::with_capacity(elems.len() + 1);
                let mut param = None;
                for (_, name) in elems {
                    let e = effect_index(effects, name)?;
                    let p = param_index(&effects[e as usize], param_str)?;
                    // One literal serves every iteration, so it can only be
                    // replaced if every effect puts the parameter in the same
                    // slot. They do when the table is one effect applied N
                    // times, which is the only way AE writes this.
                    if *param.get_or_insert(p) != p {
                        return None;
                    }
                    out.push(e);
                }
                out.push(param?);
                Some(out)
            }
        }
    }
}

fn effect_index(effects: &[ir::Effect], name: &str) -> Option<u32> {
    let mut found = None;
    for (i, e) in effects.iter().enumerate() {
        if e.name.as_deref() == Some(name) || e.match_name.as_deref() == Some(name) {
            if found.is_some() {
                return None;
            }
            found = Some(i as u32);
        }
    }
    found
}

fn param_index(effect: &ir::Effect, name: &str) -> Option<u32> {
    let mut found = None;
    for (j, p) in effect.parameters.iter().enumerate() {
        if p.name.as_deref() == Some(name) || p.match_name.as_deref() == Some(name) {
            if found.is_some() {
                return None;
            }
            found = Some(j as u32);
        }
    }
    found
}

/// Every effect-parameter reference a body makes to its own layer.
///
/// Missing one is safe: it stays a name lookup, and the name stays in the
/// table because [`mentions`] decides that from the finished text rather than
/// from this walk.
pub fn refs(body: &str) -> Vec<Ref> {
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, body, SourceType::cjs()).parse();
    if !parsed.errors.is_empty() {
        return Vec::new();
    }
    let mut out = Out::default();
    for stmt in &parsed.program.body {
        stmt_refs(stmt, &mut out);
    }
    tables(&mut out);
    out.refs
}

/// Splice resolved indices into the body, replacing each name literal.
///
/// Applied back to front so an earlier replacement cannot move a later span.
pub fn rewrite(body: &str, resolved: &[(&Ref, Vec<u32>)]) -> String {
    let mut cuts: Vec<(Span, String)> = Vec::new();
    for (r, values) in resolved {
        for (span, v) in r.spans().into_iter().zip(values) {
            cuts.push((span, v.to_string()));
        }
    }
    cuts.sort_by_key(|(s, _)| std::cmp::Reverse(s.start));
    let mut out = body.to_string();
    for (span, text) in cuts {
        out.replace_range(span.start..span.end, &text);
    }
    out
}

/// Every string literal in a body, whatever it is used for.
///
/// Deliberately lexical rather than syntactic. This decides whether a name can
/// be dropped from the payload, so a syntactic scan that missed one construct
/// would drop a name the body still looks up — a runtime failure. Scanning for
/// quotes cannot miss a construct, only misread one, and a misread keeps a
/// name that is not needed. Over-keeping costs bytes and nothing else.
///
/// Matching whole literals rather than substrings matters more than it looks:
/// `'ADBE Layer Control'` is a substring of `'ADBE Layer Control-0001'`, so a
/// `contains` test keeps every effect name whose parameter is still looked up
/// by name — on `starfish`, ten of them.
pub fn literals(body: &str) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        let Some(quote) = (matches!(c, '\'' | '"' | '`')).then_some(c) else {
            continue;
        };
        let mut lit = String::new();
        while let Some(c) = chars.next() {
            match c {
                '\\' => {
                    // Whatever it escapes, it is one character and not a close
                    // quote. Escapes do not appear in AE names.
                    if let Some(n) = chars.next() {
                        lit.push(n);
                    }
                }
                c if c == quote => break,
                c => lit.push(c),
            }
        }
        out.insert(lit);
    }
    out
}

// ---------------------------------------------------------------------------
// Finding the pattern
// ---------------------------------------------------------------------------

/// Everything one pass over a body collects.
#[derive(Default)]
struct Out {
    refs: Vec<Ref>,
    /// `var x = ['a','b']` — every all-string array literal, by binding name.
    arrays: std::collections::HashMap<String, Vec<(Span, String)>>,
    /// Every identifier reference, so a name used for anything the rewrite
    /// does not understand can veto it.
    idents: Vec<(String, Span)>,
    /// Identifier references the rewrite accounts for: `x.length`, and `x[i]`
    /// in the argument of an `effect(…)` call.
    accounted: Vec<Span>,
    /// `effect(x[i])(<param>)` — the array's name and the parameter literal.
    indexed: Vec<(String, Span, String)>,
}

fn stmt_refs<'a>(stmt: &ast::Statement<'a>, out: &mut Out) {
    match stmt {
        ast::Statement::ExpressionStatement(s) => expr_refs(&s.expression, out),
        ast::Statement::VariableDeclaration(d) => {
            for decl in &d.declarations {
                let Some(e) = &decl.init else { continue };
                if let (Some(id), ast::Expression::ArrayExpression(a)) =
                    (decl.id.get_binding_identifier(), e)
                    && let Some(elems) = all_strings(a) {
                        out.arrays.insert(id.name.to_string(), elems);
                        continue;
                    }
                expr_refs(e, out);
            }
        }
        ast::Statement::BlockStatement(b) => {
            for s in &b.body {
                stmt_refs(s, out);
            }
        }
        ast::Statement::IfStatement(s) => {
            expr_refs(&s.test, out);
            stmt_refs(&s.consequent, out);
            if let Some(a) = &s.alternate {
                stmt_refs(a, out);
            }
        }
        ast::Statement::ForStatement(s) => {
            if let Some(ast::ForStatementInit::VariableDeclaration(d)) = &s.init {
                for decl in &d.declarations {
                    if let Some(e) = &decl.init {
                        expr_refs(e, out);
                    }
                }
            }
            for e in [&s.test, &s.update].into_iter().flatten() {
                expr_refs(e, out);
            }
            stmt_refs(&s.body, out);
        }
        ast::Statement::WhileStatement(s) => {
            expr_refs(&s.test, out);
            stmt_refs(&s.body, out);
        }
        ast::Statement::ReturnStatement(s) => {
            if let Some(e) = &s.argument {
                expr_refs(e, out);
            }
        }
        ast::Statement::TryStatement(s) => {
            for st in &s.block.body {
                stmt_refs(st, out);
            }
            if let Some(h) = &s.handler {
                for st in &h.body.body {
                    stmt_refs(st, out);
                }
            }
            if let Some(f) = &s.finalizer {
                for st in &f.body {
                    stmt_refs(st, out);
                }
            }
        }
        _ => {}
    }
}

fn expr_refs<'a>(e: &ast::Expression<'a>, out: &mut Out) {
    if let ast::Expression::CallExpression(c) = e {
        if let Some(r) = as_ref(c) {
            out.refs.push(r);
            return;
        }
        if let Some((name, span, param, param_str)) = as_table_use(c) {
            out.accounted.push(span);
            out.idents.push((name.clone(), span));
            out.indexed.push((name, param, param_str));
            return;
        }
    }
    match e {
        ast::Expression::Identifier(id) => {
            out.idents.push((id.name.to_string(), span_at(id.span())));
        }
        ast::Expression::CallExpression(c) => {
            expr_refs(&c.callee, out);
            for a in &c.arguments {
                if let Some(x) = a.as_expression() {
                    expr_refs(x, out);
                }
            }
        }
        // `x.length` reads the count, which a rewrite does not change.
        ast::Expression::StaticMemberExpression(m) => {
            if let (ast::Expression::Identifier(id), "length") =
                (&m.object, m.property.name.as_str())
            {
                out.accounted.push(span_at(id.span()));
            }
            expr_refs(&m.object, out);
        }
        ast::Expression::ComputedMemberExpression(m) => {
            expr_refs(&m.object, out);
            expr_refs(&m.expression, out);
        }
        ast::Expression::ParenthesizedExpression(p) => expr_refs(&p.expression, out),
        ast::Expression::SequenceExpression(s) => {
            for x in &s.expressions {
                expr_refs(x, out);
            }
        }
        ast::Expression::AssignmentExpression(a) => expr_refs(&a.right, out),
        ast::Expression::BinaryExpression(b) => {
            expr_refs(&b.left, out);
            expr_refs(&b.right, out);
        }
        ast::Expression::LogicalExpression(l) => {
            expr_refs(&l.left, out);
            expr_refs(&l.right, out);
        }
        ast::Expression::ConditionalExpression(c) => {
            expr_refs(&c.test, out);
            expr_refs(&c.consequent, out);
            expr_refs(&c.alternate, out);
        }
        ast::Expression::UnaryExpression(u) => expr_refs(&u.argument, out),
        ast::Expression::UpdateExpression(_) => {}
        ast::Expression::ArrayExpression(a) => {
            for el in &a.elements {
                if let Some(x) = el.as_expression() {
                    expr_refs(x, out);
                }
            }
        }
        _ => {}
    }
}

/// Turn the collected array uses into [`Ref::Table`]s.
///
/// An array is only rewritten when *every* reference to it is one the rewrite
/// understands: `x.length`, or `x[i]` inside an `effect(…)` call. A name that
/// escapes anywhere else — compared, returned, passed on — could be read as a
/// string, and swapping in a number there would change what the body computes.
fn tables(out: &mut Out) {
    let indexed = std::mem::take(&mut out.indexed);
    for (name, param, param_str) in indexed {
        // One table, one parameter literal. Two uses would need two rewrites
        // of the same elements, which is a case AE does not generate.
        let Some(elems) = out.arrays.remove(&name) else {
            continue;
        };
        let escapes = out
            .idents
            .iter()
            .any(|(n, s)| *n == name && !out.accounted.contains(s));
        if escapes {
            continue;
        }
        out.refs.push(Ref::Table {
            elems,
            param,
            param_str,
        });
    }
}

/// `effect(x[…])(<string>)` — the array's name, the identifier's span, and the
/// parameter literal.
fn as_table_use<'a>(c: &ast::CallExpression<'a>) -> Option<(String, Span, Span, String)> {
    let param = only_string(&c.arguments)?;
    let ast::Expression::CallExpression(inner) = &c.callee else {
        return None;
    };
    if !is_own_effect(&inner.callee) || inner.arguments.len() != 1 {
        return None;
    }
    let ast::Expression::ComputedMemberExpression(m) = inner.arguments.first()?.as_expression()?
    else {
        return None;
    };
    let ast::Expression::Identifier(id) = &m.object else {
        return None;
    };
    Some((
        id.name.to_string(),
        span_at(id.span()),
        span_of(param),
        param.value.to_string(),
    ))
}

/// An array literal of nothing but strings.
fn all_strings<'a>(a: &ast::ArrayExpression<'a>) -> Option<Vec<(Span, String)>> {
    let mut out = Vec::with_capacity(a.elements.len());
    for el in &a.elements {
        match el.as_expression()? {
            ast::Expression::StringLiteral(s) => out.push((span_of(s), s.value.to_string())),
            _ => return None,
        }
    }
    (!out.is_empty()).then_some(out)
}

/// `effect(<string>)(<string>)`, on the owning layer.
fn as_ref<'a>(c: &ast::CallExpression<'a>) -> Option<Ref> {
    let param = only_string(&c.arguments)?;
    let ast::Expression::CallExpression(inner) = &c.callee else {
        return None;
    };
    if !is_own_effect(&inner.callee) {
        return None;
    }
    let name = only_string(&inner.arguments)?;
    Some(Ref::Direct {
        name: span_of(name),
        param: span_of(param),
        name_str: name.value.to_string(),
        param_str: param.value.to_string(),
    })
}

/// The `effect` of `effect(…)` or `thisLayer.effect(…)`, and nothing else:
/// `thisComp.layer('x').effect(…)` reaches a different layer's table.
fn is_own_effect<'a>(callee: &ast::Expression<'a>) -> bool {
    match callee {
        ast::Expression::Identifier(id) => id.name == "effect",
        ast::Expression::StaticMemberExpression(m) => {
            m.property.name == "effect"
                && matches!(&m.object, ast::Expression::Identifier(o) if o.name == "thisLayer")
        }
        _ => false,
    }
}

fn only_string<'a, 'b>(
    args: &'b oxc_allocator::Vec<'a, ast::Argument<'a>>,
) -> Option<&'b ast::StringLiteral<'a>> {
    if args.len() != 1 {
        return None;
    }
    match args.first()?.as_expression()? {
        ast::Expression::StringLiteral(s) => Some(s),
        _ => None,
    }
}

fn span_of(s: &ast::StringLiteral) -> Span {
    span_at(s.span())
}

fn span_at(sp: oxc_span::Span) -> Span {
    Span {
        start: sp.start as usize,
        end: sp.end as usize,
    }
}

// ---------------------------------------------------------------------------
// Branch folding
// ---------------------------------------------------------------------------

/// An `if` whose test the compiler may be able to decide.
///
/// Bodymovin guards a great deal behind conditions made entirely of things the
/// compiler knows — an effect checkbox, a keyframe count — and leaves the guard
/// in the shipped body. Paying for it at runtime costs the test, the string
/// literals in it, and the effect names those literals keep alive through the
/// lexical rule in `prune_effect_names`.
pub struct Branch {
    /// The whole `if (…) … else …` statement.
    stmt: Span,
    /// Its test, as source, so it can be evaluated on its own.
    pub test: String,
    taken: Span,
    other: Option<Span>,
}

impl Branch {
    /// The source that replaces the statement, once the test is `decided`.
    ///
    /// An arm is usually a block, and splicing the block in whole would leave
    /// its braces behind wrapping nothing — a bare `{ … }` where a statement
    /// used to be guarded. `arm_span` reaches inside it, so what lands is the
    /// statements themselves.
    pub fn arm(&self, body: &str, decided: bool) -> Option<(Span, String)> {
        let arm = if decided { self.taken } else { self.other? };
        // Nothing at all when the arm was an empty block: the `if` goes and
        // leaves no trace, which is what it did.
        if arm.start >= arm.end {
            return Some((self.stmt, String::new()));
        }
        Some((self.stmt, dedent(&body[arm.start..arm.end])))
    }
}

/// Every top-level `if` whose test reads nothing the body itself defines.
///
/// That restriction is what makes evaluating a test in isolation sound: one
/// mentioning a local would need the statements before it replayed, and this
/// pass does not run the body. Bodymovin's guards read `thisProperty` and
/// `effect(…)` and nothing else, which is the case worth having.
pub fn branches(body: &str) -> Vec<Branch> {
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, body, SourceType::cjs()).parse();
    if !parsed.errors.is_empty() {
        return Vec::new();
    }
    let mut locals = std::collections::BTreeSet::new();
    for stmt in &parsed.program.body {
        if let ast::Statement::VariableDeclaration(d) = stmt {
            for decl in &d.declarations {
                if let Some(id) = decl.id.get_binding_identifier() {
                    locals.insert(id.name.to_string());
                }
            }
        }
    }
    let mut out = Vec::new();
    for stmt in &parsed.program.body {
        let ast::Statement::IfStatement(s) = stmt else {
            continue;
        };
        let test = span_at(s.test.span());
        let src = &body[test.start..test.end];
        if locals.iter().any(|l| mentions_word(src, l)) {
            continue;
        }
        out.push(Branch {
            stmt: span_at(s.span()),
            test: src.to_string(),
            taken: arm_span(&s.consequent),
            other: s.alternate.as_ref().map(arm_span),
        });
    }
    out
}

/// An arm's statements, without the block that held them.
fn arm_span(stmt: &ast::Statement) -> Span {
    let ast::Statement::BlockStatement(b) = stmt else {
        return span_at(stmt.span());
    };
    match (b.body.first(), b.body.last()) {
        (Some(f), Some(l)) => Span {
            start: f.span().start as usize,
            end: l.span().end as usize,
        },
        // An empty block: a span that selects nothing.
        _ => Span { start: 0, end: 0 },
    }
}

/// Pull the arm back out one level of block indentation.
///
/// Only the continuation lines need it — the first begins where the statement
/// does, which is where the `if` began. Purely for the unminified form, which
/// is what compiler changes are reviewed in.
fn dedent(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    for (i, line) in src.lines().enumerate() {
        if i > 0 {
            out.push('\n');
            out.push_str(line.strip_prefix("    ").unwrap_or(line));
        } else {
            out.push_str(line);
        }
    }
    out
}

/// Whether `src` uses `word` as an identifier rather than inside a longer one.
fn mentions_word(src: &str, word: &str) -> bool {
    let edge =
        |c: Option<char>| !matches!(c, Some(c) if c.is_alphanumeric() || c == '_' || c == '$');
    src.match_indices(word).any(|(i, _)| {
        edge(src[..i].chars().next_back()) && edge(src[i + word.len()..].chars().next())
    })
}

/// Splice decided branches in, back to front so earlier spans stay valid.
pub fn take_branches(body: &str, decided: &[(Span, String)]) -> String {
    let mut cuts = decided.to_vec();
    cuts.sort_by_key(|(s, _)| std::cmp::Reverse(s.start));
    let mut out = body.to_string();
    for (span, text) in cuts {
        out.replace_range(span.start..span.end, &text);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_finds_a_reference_to_the_owning_layer() {
        let body =
            "var $bm_rt;\n$bm_rt = effect('Position - Overshoot')('ADBE Slider Control-0001');";
        let r = refs(body);
        assert_eq!(r.len(), 1);
        assert!(matches!(&r[0], Ref::Direct { name_str, param_str, .. }
            if name_str == "Position - Overshoot" && param_str == "ADBE Slider Control-0001"));
    }

    #[test]
    fn it_finds_one_through_this_layer() {
        let body = "var $bm_rt;\n$bm_rt = thisLayer.effect('Trace')('Progress');";
        assert_eq!(refs(body).len(), 1);
    }

    #[test]
    fn it_leaves_another_layers_effects_alone() {
        // Resolving this against the owning layer's table would point it at
        // whatever happens to sit at that index on the wrong layer.
        let body =
            "var $bm_rt;\n$bm_rt = thisComp.layer('traceNull').effect('Trace Path')('Progress');";
        assert!(refs(body).is_empty());
    }

    #[test]
    fn it_finds_references_nested_in_statements() {
        let body = r#"
var $bm_rt;
try {
    var a = div(effect('Position - Bounce')('ADBE Slider Control-0001'), 20);
    if (a > 0) { $bm_rt = effect('Position - Friction')('ADBE Slider Control-0001'); }
} catch (e) { $bm_rt = value; }
"#;
        let found = refs(body);
        let names: Vec<&str> = found
            .iter()
            .filter_map(|r| match r {
                Ref::Direct { name_str, .. } => Some(name_str.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(names, ["Position - Bounce", "Position - Friction"]);
    }

    /// The shape After Effects generates when one effect is applied across a
    /// set of layers — `lights` and `starfish` both carry it, and the names in
    /// it are the largest thing left in their string tables.
    const TABLE: &str = r#"
var $bm_rt;
var nullLayerNames = ['Shape Layer 1: Path 1 [1.0]', 'Shape Layer 1: Path 1 [1.1]'];
var out = [];
for (var i = 0; i < nullLayerNames.length; i++) {
    out.push(effect(nullLayerNames[i])('ADBE Layer Control-0001'));
}
$bm_rt = out;
"#;

    #[test]
    fn a_table_of_names_indexed_into_effect_is_one_reference() {
        let found = refs(TABLE);
        assert_eq!(found.len(), 1);
        assert!(matches!(&found[0], Ref::Table { elems, param_str, .. }
            if elems.len() == 2 && param_str == "ADBE Layer Control-0001"));
    }

    #[test]
    fn a_table_resolves_every_element_and_the_shared_parameter() {
        let fx = [
            effect("Shape Layer 1: Path 1 [1.0]", &["ADBE Layer Control-0001"]),
            effect("other", &["x"]),
            effect("Shape Layer 1: Path 1 [1.1]", &["ADBE Layer Control-0001"]),
        ];
        let found = refs(TABLE);
        assert_eq!(found[0].resolve(&fx), Some(vec![0, 2, 0]));
    }

    #[test]
    fn a_table_whose_effects_disagree_about_the_parameter_slot_is_refused() {
        // One literal serves every iteration, so it can only be replaced if
        // every effect puts the parameter in the same slot.
        let fx = [
            effect("Shape Layer 1: Path 1 [1.0]", &["ADBE Layer Control-0001"]),
            effect(
                "Shape Layer 1: Path 1 [1.1]",
                &["x", "ADBE Layer Control-0001"],
            ),
        ];
        assert_eq!(refs(TABLE)[0].resolve(&fx), None);
    }

    #[test]
    fn a_table_rewrites_to_indices() {
        let fx = [
            effect("Shape Layer 1: Path 1 [1.0]", &["ADBE Layer Control-0001"]),
            effect("Shape Layer 1: Path 1 [1.1]", &["ADBE Layer Control-0001"]),
        ];
        let found = refs(TABLE);
        let out = rewrite(TABLE, &[(&found[0], found[0].resolve(&fx).unwrap())]);
        assert!(out.contains("var nullLayerNames = [0, 1];"), "got {out}");
        assert!(out.contains("effect(nullLayerNames[i])(0)"), "got {out}");
    }

    #[test]
    fn a_table_whose_names_escape_is_left_alone() {
        // The array is also read as a string here, so swapping in numbers
        // would change what the body computes.
        let body = r#"
var $bm_rt;
var names = ['a', 'b'];
var label = names[0] + '!';
for (var i = 0; i < names.length; i++) { effect(names[i])('p'); }
$bm_rt = label;
"#;
        assert!(refs(body).is_empty());
    }

    #[test]
    fn an_array_nothing_indexes_into_effect_is_not_a_reference() {
        let body = "var $bm_rt;\nvar names = ['a', 'b'];\n$bm_rt = names.length;";
        assert!(refs(body).is_empty());
    }

    fn effect(name: &str, params: &[&str]) -> ir::Effect {
        ir::Effect {
            name: Some(name.to_string()),
            match_name: None,
            ty: 5,
            index: None,
            enabled: true,
            parameters: params
                .iter()
                .map(|p| ir::EffectParam {
                    name: Some(p.to_string()),
                    match_name: None,
                    ty: 10,
                    index: None,
                    value: ir::EffectValue::Scalar(ir::Property::Static(0.0)),
                })
                .collect(),
        }
    }

    #[test]
    fn a_name_is_kept_only_when_a_whole_literal_matches_it() {
        let lits = literals("effect(names[i])('ADBE Layer Control-0001')");
        assert!(lits.contains("ADBE Layer Control-0001"));
        // The effect's own name is a *substring* of its parameter's. A
        // `contains` test would keep it, and nothing looks it up.
        assert!(!lits.contains("ADBE Layer Control"));
    }

    #[test]
    fn literals_are_found_whatever_quotes_them() {
        let lits = literals("var a = 'x', b = \"y\", c = `z`;");
        assert_eq!(
            lits.iter().map(String::as_str).collect::<Vec<_>>(),
            ["x", "y", "z"]
        );
    }

    #[test]
    fn an_escaped_quote_does_not_end_a_literal() {
        let lits = literals(r#"var a = 'it\'s', b = 'after';"#);
        assert!(lits.contains("it's"), "got {lits:?}");
        assert!(lits.contains("after"), "got {lits:?}");
    }

    #[test]
    fn rewriting_replaces_both_literals() {
        let body =
            "var $bm_rt;\n$bm_rt = effect('Position - Overshoot')('ADBE Slider Control-0001');";
        let r = refs(body);
        let out = rewrite(body, &[(&r[0], vec![2, 0])]);
        assert_eq!(out, "var $bm_rt;\n$bm_rt = effect(2)(0);");
    }

    #[test]
    fn rewriting_several_keeps_every_span_aligned() {
        let body = "var $bm_rt;\n$bm_rt = sum(effect('A')('x'), effect('B')('y'));";
        let r = refs(body);
        let out = rewrite(body, &[(&r[0], vec![0, 1]), (&r[1], vec![2, 3])]);
        assert_eq!(
            out,
            "var $bm_rt;\n$bm_rt = sum(effect(0)(1), effect(2)(3));"
        );
    }
}
