//! Readable formatting for unminified output.
//!
//! Shipped output is one long line, which makes a compiler change invisible in
//! review. The same module printed through here is line-oriented: one SVG
//! element per line, one binding per line, so a diff points at the attribute or
//! keyframe that actually moved.
//!
//! This is a review artifact, not a second code path — the emitted text is the
//! same module, only whitespace differs.

use serde_json::Value;

/// Width past which a JSON container is broken across lines. Small arrays —
/// coordinate pairs, easing handles, one binding's arguments — stay inline,
/// which is both shorter and easier to scan than one element per line.
const INLINE_WIDTH: usize = 96;

/// Render a JSON value with containers kept inline while they fit.
pub fn json(v: &Value, indent: usize) -> String {
    let mut out = String::new();
    write_json(v, indent, &mut out);
    out
}

fn write_json(v: &Value, indent: usize, out: &mut String) {
    let compact = v.to_string();
    if compact.len() + indent <= INLINE_WIDTH || !v.is_array() && !v.is_object() {
        out.push_str(&compact);
        return;
    }
    let pad = "  ".repeat(indent / 2 + 1);
    let close_pad = "  ".repeat(indent / 2);

    // Coordinate and keyframe data is all numbers; wrapping it like prose keeps
    // it compact and still gives a diff a narrow line to point at. One number
    // per line would make a path unreadable.
    if let Value::Array(items) = v {
        if items.iter().all(Value::is_number) {
            out.push_str("[\n");
            out.push_str(&pad);
            let mut col = pad.len();
            for (i, item) in items.iter().enumerate() {
                let t = item.to_string();
                if col + t.len() > INLINE_WIDTH && col > pad.len() {
                    out.push('\n');
                    out.push_str(&pad);
                    col = pad.len();
                }
                out.push_str(&t);
                col += t.len();
                if i + 1 < items.len() {
                    out.push(',');
                    col += 1;
                    // The space is only emitted when the next number stays on
                    // this line, so no line ends in trailing whitespace.
                    let next = items[i + 1].to_string();
                    if col + 1 + next.len() <= INLINE_WIDTH {
                        out.push(' ');
                        col += 1;
                    }
                }
            }
            out.push('\n');
            out.push_str(&close_pad);
            out.push(']');
            return;
        }
    }

    match v {
        Value::Array(items) => {
            out.push_str("[\n");
            for (i, item) in items.iter().enumerate() {
                out.push_str(&pad);
                write_json(item, indent + 2, out);
                if i + 1 < items.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str(&close_pad);
            out.push(']');
        }
        Value::Object(map) => {
            out.push_str("{\n");
            for (i, (k, val)) in map.iter().enumerate() {
                out.push_str(&pad);
                out.push('"');
                out.push_str(k);
                out.push_str("\": ");
                write_json(val, indent + 2, out);
                if i + 1 < map.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str(&close_pad);
            out.push('}');
        }
        _ => out.push_str(&compact),
    }
}

/// Render baked markup as an indented JS string concatenation — one element per
/// line, nested by depth. Concatenating the parts reproduces the original
/// string exactly.
pub fn markup(m: &str, quote: impl Fn(&str) -> String) -> String {
    let tags = split_tags(m);
    let mut out = String::new();
    let mut depth = 0usize;
    for (i, tag) in tags.iter().enumerate() {
        let closing = tag.starts_with("</");
        if closing {
            depth = depth.saturating_sub(1);
        }
        out.push_str(&"  ".repeat(depth + 1));
        out.push_str(&quote(tag));
        if i + 1 < tags.len() {
            out.push_str(" +");
        }
        out.push('\n');
        if !closing && tag.starts_with('<') && !tag.ends_with("/>") {
            depth += 1;
        }
    }
    out
}

/// Indent markup one element per line, as plain SVG. Same nesting as
/// [`markup`] but without the JS string quoting — this is the standalone
/// document template.
pub fn markup_plain(m: &str) -> String {
    let tags = split_tags(m);
    let mut out = String::new();
    let mut depth = 0usize;
    for tag in &tags {
        let closing = tag.starts_with("</");
        if closing {
            depth = depth.saturating_sub(1);
        }
        out.push_str(&"  ".repeat(depth));
        out.push_str(tag);
        out.push('\n');
        if !closing && tag.starts_with('<') && !tag.ends_with("/>") {
            depth += 1;
        }
    }
    out
}

/// Split markup after every tag. Generated markup has no text nodes, and `>`
/// inside an attribute value is quoted, so this is exact.
fn split_tags(m: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for ch in m.chars() {
        cur.push(ch);
        match ch {
            '"' => quoted = !quoted,
            '>' if !quoted => out.push(std::mem::take(&mut cur)),
            _ => {}
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(s: &str) -> String {
        format!("'{s}'")
    }

    #[test]
    fn markup_nests_by_element_depth() {
        let m = r#"<svg a="1"><g b="2"><rect c="3"/></g></svg>"#;
        assert_eq!(
            markup(m, q),
            "  '<svg a=\"1\">' +\n    '<g b=\"2\">' +\n      '<rect c=\"3\"/>' +\n    '</g>' +\n  '</svg>'\n"
        );
    }

    #[test]
    fn concatenating_the_parts_reproduces_the_markup() {
        let m = r#"<svg viewBox="0 0 2 2"><path d="M0,0L1,1Z"/><g/></svg>"#;
        let joined: String = split_tags(m).concat();
        assert_eq!(joined, m);
    }

    #[test]
    fn a_quoted_angle_bracket_does_not_split_a_tag() {
        let m = r#"<svg t="a>b"><g/></svg>"#;
        assert_eq!(split_tags(m), vec![r#"<svg t="a>b">"#, "<g/>", "</svg>"]);
    }

    #[test]
    fn small_containers_stay_inline() {
        let v: Value = serde_json::from_str(r#"{"t":[0,10],"v":[1,2]}"#).unwrap();
        assert_eq!(json(&v, 0), r#"{"t":[0,10],"v":[1,2]}"#);
    }

    #[test]
    fn wide_containers_break_one_entry_per_line() {
        let v: Value = serde_json::from_str(
            r#"{"b":[[0,1,"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"],[1,2,"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"]]}"#,
        )
        .unwrap();
        let s = json(&v, 0);
        assert!(s.contains("\n"), "expected a multi-line rendering, got {s}");
        // Inner arrays still fit, so they stay on one line each.
        assert!(s.contains(r#"[0,1,"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"]"#), "{s}");
    }
}
