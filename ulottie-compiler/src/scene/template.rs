//! Factoring repeated subtrees out of the inlined markup.
//!
//! Baking the whole document into the module is the right call for an ordinary
//! animation: it is one string, it needs no construction code, and it is
//! directly server-renderable. It is the wrong call when a precomp is instanced
//! forty-six times and the same subtree is written out forty-six times.
//!
//! So it is a budget, not a rule. Past [`DEFAULT_INLINE_LIMIT`] the emitter
//! keeps one copy of each repeated subtree and leaves a placeholder at each
//! occurrence; the runtime expands them *before* indexing elements, so the
//! document-order indices the compiler assigned still line up.
//!
//! The standalone document template ([`Scene::markup`]) is unaffected — it is
//! always fully expanded, whatever the module chose to inline.

use std::collections::HashMap;

use super::{Planner, El};

/// Don't factor out anything smaller than this. A placeholder is ~19 bytes and
/// the table entry pays for itself after the second occurrence, so the floor
/// only has to be comfortably above the placeholder.
const MIN_TEMPLATE_BYTES: usize = 40;

impl Planner<'_> {
    /// `(inlined markup, template table)`.
    ///
    /// Returns the markup unchanged when it fits the budget or when nothing
    /// repeats.
    pub(super) fn templated(
        &self,
        markup: &str,
        roots: &[usize],
        payload: &crate::data::Payload,
    ) -> (String, Vec<String>) {
        if markup.len() <= self.inline_limit {
            return (markup.to_string(), Vec::new());
        }

        // Serialize every subtree once. Identical markup means interchangeable
        // subtrees — bindings address elements by index, not by identity, so a
        // shared initial DOM is all that has to match.
        let mut cache: HashMap<usize, String> = HashMap::new();
        for r in roots {
            self.subtree(*r, &mut cache);
        }

        let mut counts: HashMap<&str, usize> = HashMap::new();
        for text in cache.values() {
            *counts.entry(text.as_str()).or_insert(0) += 1;
        }

        let worth_it = |text: &str| {
            text.len() >= MIN_TEMPLATE_BYTES
                && counts.get(text).copied().unwrap_or(0) >= 2
                // A subtree carrying a generated id cannot be cloned: every
                // copy would repeat the id, and `url(#…)` resolves
                // document-wide.
                && !text.contains(super::svg::ID_MARK)
        };

        let mut ids: HashMap<String, usize> = HashMap::new();
        let mut table: Vec<String> = Vec::new();
        let mut out = String::with_capacity(markup.len());
        out.push_str(&format!(
            "<svg viewBox=\"0 0 {} {}\" width=\"100%\" height=\"100%\" \
             preserveAspectRatio=\"xMidYMid meet\" style=\"overflow:hidden\">",
            payload.c.w, payload.c.h
        ));
        for r in roots {
            self.emit_templated(*r, &cache, &worth_it, &mut ids, &mut table, &mut out);
        }
        out.push_str("</svg>");

        if std::env::var("ULOTTIE_DEBUG_TPL").is_ok() {
            let mut c: Vec<_> = counts.iter().filter(|(t, n)| **n >= 2 && t.len() >= 40).collect();
            c.sort_by_key(|(t, n)| std::cmp::Reverse(t.len() * **n));
            eprintln!("markup {} B, limit {}, candidates:", markup.len(), self.inline_limit);
            for (t, n) in c.iter().take(5) {
                eprintln!("  {n}x {} B  worth={}  {}", t.len(), (worth_it)(t), &t[..t.len().min(70)]);
            }
            eprintln!("  table: {} entries", table.len());
        }
        if table.is_empty() {
            return (markup.to_string(), Vec::new());
        }
        (out, table)
    }

    /// Memoized subtree markup.
    fn subtree(&self, id: usize, cache: &mut HashMap<usize, String>) -> String {
        if let Some(t) = cache.get(&id) {
            return t.clone();
        }
        let e = &self.els[id];
        // A precomp instance is already a placeholder; leave it alone.
        if let Some(asset) = e.instance {
            let s = super::placeholder(self.assets[asset as usize].template);
            cache.insert(id, s.clone());
            return s;
        }
        let mut s = String::new();
        s.push('<');
        s.push_str(e.tag);
        for (k, v) in &e.attrs {
            s.push(' ');
            s.push_str(k);
            s.push_str("=\"");
            s.push_str(v);
            s.push('"');
        }
        if e.children.is_empty() {
            s.push_str("/>");
        } else {
            s.push('>');
            let kids: Vec<usize> = e.children.clone();
            for c in kids {
                let t = self.subtree(c, cache);
                s.push_str(&t);
            }
            s.push_str("</");
            s.push_str(e.tag);
            s.push('>');
        }
        cache.insert(id, s.clone());
        s
    }

    /// Emit a subtree, replacing the topmost repeated nodes with placeholders.
    /// Templates never nest: once a node is factored out, its descendants are
    /// part of that one template, so expansion is a single pass.
    fn emit_templated(
        &self,
        id: usize,
        cache: &HashMap<usize, String>,
        worth_it: &impl Fn(&str) -> bool,
        ids: &mut HashMap<String, usize>,
        table: &mut Vec<String>,
        out: &mut String,
    ) {
        let text = &cache[&id];
        if worth_it(text) {
            let next = ids.len();
            let slot = *ids.entry(text.clone()).or_insert_with(|| {
                table.push(text.clone());
                next
            });
            out.push_str(&format!("<g data-t=\"{slot}\"/>"));
            return;
        }
        let e: &El = &self.els[id];
        if let Some(asset) = e.instance {
            out.push_str(&super::placeholder(self.assets[asset as usize].template));
            return;
        }
        out.push('<');
        out.push_str(e.tag);
        for (k, v) in &e.attrs {
            out.push(' ');
            out.push_str(k);
            out.push_str("=\"");
            out.push_str(v);
            out.push('"');
        }
        if e.children.is_empty() {
            out.push_str("/>");
            return;
        }
        out.push('>');
        for c in &e.children {
            self.emit_templated(*c, cache, worth_it, ids, table, out);
        }
        out.push_str("</");
        out.push_str(e.tag);
        out.push('>');
    }
}
