//! Text layers: fonts, glyph outlines, and the layout that turns a text
//! document into shape geometry at compile time.
//!
//! Lottie spells a text layer as a *document* (the string, font, size, colour,
//! justification, tracking) plus *glyph outlines* carried per character in the
//! composition's `chars` list. lottie-web lays the string out at run time —
//! `TextProperty.completeTextData` for the metrics and
//! `TextAnimatorProperty.getMeasures` for the per-letter transforms — and
//! instances one `<g>` per character: the glyph's own shape tree, its group
//! transform's scale overwritten to the font size (`buildShapeData` — the
//! `tr` that lottie-web's data completion injects when the export carries
//! none), translated to the letter's pen position.
//!
//! For a static document with no *animators* — every file that does not
//! animate the text itself — all of those numbers are frame-invariant, so the
//! whole layer lowers to ordinary shapes here: one group per character
//! holding the glyph's outlines, a fill from the document colour, and the
//! letter's position. Downstream (planner, bake, codegen) never learns the
//! layer was text.
//!
//! The layout mirrors lottie-web exactly, including three quirks worth
//! naming: a first-newline `+1px` on the line advance; the advance *after*
//! the last character of a line counting toward the width justification
//! centres on; and a line-width accumulator that starts at `−tracking` for
//! the first line but `−2·tracking` for every line after.

use serde::{Deserialize, Serialize};

use super::graphic::GraphicElement;
use super::property::Property;

// ---------------------------------------------------------------------------
// Document AST
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Fonts {
    pub list: Vec<Font>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Font {
    #[serde(rename = "fName")]
    pub f_name: String,
    #[serde(rename = "fFamily")]
    pub f_family: String,
    #[serde(rename = "fStyle")]
    pub f_style: String,
    pub ascent: f64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct GlyphChar {
    pub ch: String,
    /// Advance width in 100-unit font space.
    pub w: f64,
    #[serde(rename = "fFamily")]
    pub f_family: Option<String>,
    pub style: Option<String>,
    pub size: Option<f64>,
    /// `t: 1` marks a precomposed (animated) glyph rather than outlines.
    pub t: Option<u8>,
    pub data: GlyphData,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct GlyphData {
    pub shapes: Option<Vec<GraphicElement>>,
}

/// A text layer's `t` block.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TextData {
    /// The document data: the string and its formatting, possibly keyframed.
    pub d: TextDocument,
    /// Text animators. Not implemented; a non-empty list is a refusal.
    #[serde(default)]
    pub a: Option<serde_json::Value>,
    /// More options: anchor grouping `g` and alignment `a`.
    pub m: Option<MoreOptions>,
    /// Path options (text on a path). A set mask is a refusal.
    pub p: Option<serde_json::Value>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TextDocument {
    /// 1 when the document data itself is keyframed (the text or its
    /// formatting changes over time). A refusal.
    #[serde(default)]
    pub a: Option<u8>,
    pub k: serde_json::Value,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MoreOptions {
    pub g: Option<u8>,
    /// `[x, y]` alignment in percent. The two translates it contributes
    /// cancel for un-pathed text, so any static value is fine.
    pub a: Option<Property>,
}

/// One keyframe's document state (`d.k[i].s`).
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DocState {
    #[serde(rename = "t")]
    pub text: String,
    /// Font size.
    #[serde(rename = "s")]
    pub size: f64,
    #[serde(rename = "f")]
    pub font: String,
    /// Justification: 0 left, 1 right, 2 center.
    #[serde(default)]
    pub j: Option<u8>,
    /// Tracking, per-mille of the font size.
    #[serde(default)]
    pub tr: Option<f64>,
    /// Line height.
    #[serde(default)]
    pub lh: Option<f64>,
    /// Line spacing (shifts every line up by this much).
    #[serde(rename = "ls", default)]
    pub ls: Option<f64>,
    /// Fill colour, 0–1 rgb (a fourth component, when present, is dropped —
    /// lottie-web's `buildColor` reads three).
    #[serde(default)]
    pub fc: Option<Vec<f64>>,
    /// Stroke colour; a stroke is a refusal.
    #[serde(default)]
    pub sc: Option<Vec<f64>>,
    #[serde(default)]
    pub sw: Option<f64>,
    /// A text box (`sz`) wraps text; a refusal.
    #[serde(default)]
    pub sz: Option<Vec<f64>>,
    /// Box position `[x, y]`; shifts the text right and down by the font's
    /// ascent.
    #[serde(default)]
    pub ps: Option<Vec<f64>>,
}

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

/// Why a text layer could not be laid out. `support::scan` reaches text
/// through the same [`text_shapes`] call, so the gate and the lowering cannot
/// disagree about what is supported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextRefusal {
    /// No `chars` list (or no matching font), so there are no outlines to
    /// instance.
    NoChars,
    /// The document data is keyframed (`d.a == 1`).
    AnimatedDocument,
    /// The layer carries text animators.
    Animators,
    /// The document is boxed (`sz`) and would need word wrap.
    Box,
    /// The document carries a stroke.
    Stroke,
    /// The layer puts the text on a path.
    Path,
    /// A character of the string has no entry in `chars`.
    GlyphMissing(String),
}

impl TextRefusal {
    pub fn feature_name(&self) -> &'static str {
        match self {
            TextRefusal::NoChars => "text-no-chars",
            TextRefusal::AnimatedDocument => "text-animated",
            TextRefusal::Animators => "text-animators",
            TextRefusal::Box => "text-box",
            TextRefusal::Stroke => "text-stroke",
            TextRefusal::Path => "text-path",
            TextRefusal::GlyphMissing(_) => "text-glyph-missing",
        }
    }

    pub fn effect(&self) -> &'static str {
        match self {
            TextRefusal::NoChars => "the text is not drawn (no glyph outlines)",
            TextRefusal::AnimatedDocument => "the text is drawn as its first keyframe",
            TextRefusal::Animators => "the text is drawn without its animators",
            TextRefusal::Box => "the text is drawn without wrapping to its box",
            TextRefusal::Stroke => "the text is drawn without its stroke",
            TextRefusal::Path => "the text is drawn on a straight baseline",
            TextRefusal::GlyphMissing(_) => "the text is not drawn (a character has no outline)",
        }
    }
}

/// Lay out `state` against `chars`/`fonts`, mirroring lottie-web's
/// `completeTextData` + `getMeasures` for the no-animator, no-path case:
/// every offset pair the full machinery applies cancels, and what remains per
/// letter is a pure translate.
fn layout(state: &DocState, chars: &[GlyphChar], fonts: &[Font]) -> Result<Vec<(usize, f64, f64)>, TextRefusal> {
    if chars.is_empty() {
        return Err(TextRefusal::NoChars);
    }
    if state.sz.is_some() {
        return Err(TextRefusal::Box);
    }
    if state.sc.is_some() || state.sw.is_some() {
        return Err(TextRefusal::Stroke);
    }
    let font = fonts
        .iter()
        .find(|f| f.f_name == state.font)
        .ok_or(TextRefusal::NoChars)?;

    let size = state.size;
    let tracking = state.tr.unwrap_or(0.0) / 1000.0 * size;
    let line_height = state.lh.filter(|&lh| lh != 0.0).unwrap_or(size * 1.2);
    let ascent = font.ascent * size / 100.0;
    let ls = state.ls.unwrap_or(0.0);

    // Glyph lookup matches (character, style, family) the way lottie-web's
    // `getCharData` does; entries without metadata match on the character
    // alone. A miss is a refusal — lottie-web crashes there.
    let any_meta = chars.iter().any(|g| g.style.is_some() || g.f_family.is_some());
    let glyph_index = |c: &str| -> Option<usize> {
        chars
            .iter()
            .position(|g| {
                g.ch == c
                    && (!any_meta
                        || (g.style.as_deref() == Some(font.f_style.as_str())
                            && g.f_family.as_deref() == Some(font.f_family.as_str())))
            })
            .or_else(|| {
                if any_meta {
                    chars.iter().position(|g| g.ch == c)
                } else {
                    None
                }
            })
    };

    // Line widths first, the way `completeTextData` accumulates them — a
    // separate pass, because justification needs the *final* width of every
    // line (including the advance after its last character): spaces hold
    // their width back until the next non-space character spends it, and
    // the accumulator starts at `−tracking` for the first line but
    // `−2·tracking` for every one after.
    let mut line_widths: Vec<f64> = vec![-tracking];
    let mut uncollapsed = 0.0;
    let mut indices: Vec<Option<usize>> = Vec::with_capacity(state.text.chars().count());
    for c in state.text.chars() {
        if c == '\r' || c == '\u{3}' {
            line_widths.push(-2.0 * tracking);
            uncollapsed = 0.0;
            indices.push(None);
            continue;
        }
        let gi = glyph_index(&c.to_string()).ok_or_else(|| TextRefusal::GlyphMissing(c.to_string()))?;
        let advance = chars[gi].w * size / 100.0;
        if c == ' ' {
            uncollapsed += advance + tracking;
        } else {
            *line_widths.last_mut().unwrap() += advance + tracking + uncollapsed;
            uncollapsed = 0.0;
        }
        indices.push(Some(gi));
    }
    let box_width = line_widths.iter().cloned().fold(f64::MIN, f64::max);

    // Then the placement pass (`getMeasures` with no animators and no path:
    // every offset pair the full machinery applies cancels, and what remains
    // per letter is a pure translate).
    let mut placed: Vec<(usize, f64, f64)> = Vec::new();
    let mut x = 0.0f64;
    let mut y = 0.0f64;
    let mut first_line = true;
    let mut line = 0usize;
    for (_c, gi) in state.text.chars().zip(indices) {
        let Some(gi) = gi else {
            // A newline: a first-line-only +1px rides the line advance.
            x = 0.0;
            y += line_height;
            if first_line {
                y += 1.0;
                first_line = false;
            }
            line += 1;
            continue;
        };
        let advance = chars[gi].w * size / 100.0;
        let justify = match state.j.unwrap_or(0) {
            1 => -box_width + (box_width - line_widths[line]),
            2 => -box_width / 2.0 + (box_width - line_widths[line]) / 2.0,
            _ => 0.0,
        };
        let ps = state.ps.as_ref();
        let (px, py) = ps.map_or((0.0, 0.0), |p| {
            (p.first().copied().unwrap_or(0.0), p[1.min(p.len() - 1)])
        });
        placed.push((
            gi,
            x + justify + px,
            y + py + if ps.is_some() { ascent } else { 0.0 } - ls,
        ));
        x += advance + tracking;
    }
    Ok(placed)
}

/// Read the document state out of `d.k`, static only. `s` sits on the single
/// keyframe (`k: [{t, s}]`) or — for `a: 0` — directly on a bare `k` object.
fn document_state(d: &TextDocument) -> Result<DocState, TextRefusal> {
    if d.a == Some(1) {
        return Err(TextRefusal::AnimatedDocument);
    }
    let k = &d.k;
    let state_json = if k.is_array() {
        k.as_array()
            .and_then(|a| a.first())
            .and_then(|kf| kf.get("s"))
            .ok_or(TextRefusal::AnimatedDocument)?
    } else {
        k.get("s").ok_or(TextRefusal::AnimatedDocument)?
    };
    serde_json::from_value(state_json.clone()).map_err(|_| TextRefusal::AnimatedDocument)
}

/// The shape tree a text layer lowers to: one group per character holding the
/// glyph's outlines, the document fill, and the letter's position.
///
/// `Err` means unsupported (see [`TextRefusal`]); the caller decides whether
/// that is a refusal or an accepted degradation.
pub fn text_shapes(
    t: &TextData,
    chars: &[GlyphChar],
    fonts: &[Font],
) -> Result<Vec<GraphicElement>, TextRefusal> {
    if t.a
        .as_ref()
        .is_some_and(|a| a.as_array().is_some_and(|a| !a.is_empty()))
    {
        return Err(TextRefusal::Animators);
    }
    // Path options: an object carrying a mask is text-on-a-path.
    if let Some(p) = &t.p
        && p.as_object().is_some_and(|o| o.contains_key("m"))
    {
        return Err(TextRefusal::Path);
    }
    let state = document_state(&t.d)?;
    let placed = layout(&state, chars, fonts)?;

    let fill_colour: Vec<f64> = match &state.fc {
        Some(fc) if fc.len() >= 3 => vec![fc[0], fc[1], fc[2]],
        // No fill colour: lottie-web paints transparent.
        _ => vec![0.0, 0.0, 0.0, 0.0],
    };

    let mut out = Vec::with_capacity(placed.len());
    for (gi, tx, ty) in &placed {
        let glyph = &chars[*gi];
        let Some(shapes) = glyph.data.shapes.clone() else {
            continue;
        };
        let mut it: Vec<GraphicElement> = Vec::with_capacity(shapes.len() + 2);
        for mut shape in shapes {
            // `buildShapeData` overwrites the glyph group's scale with the
            // font size — the `tr` lottie-web's data completion appends when
            // the export carries none.
            if let GraphicElement::Group { ref mut it, .. } = shape {
                match it.last_mut() {
                    Some(GraphicElement::Transform { s, .. }) => {
                        *s = Some(static_prop(vec![state.size, state.size]));
                    }
                    _ => it.push(scale_transform(state.size)),
                }
            }
            it.push(shape);
        }
        it.push(GraphicElement::Fill {
            name: None,
            hidden: false,
            c: static_prop(fill_colour.clone()),
            o: Some(static_prop_num(100.0)),
            r: None,
            bm: None,
            match_name: None,
        });
        it.push(GraphicElement::Transform {
            name: None,
            hidden: false,
            p: Some(static_prop(vec![*tx, *ty])),
            a: Some(static_prop(vec![0.0, 0.0])),
            s: Some(static_prop(vec![100.0, 100.0])),
            r: Some(static_prop_num(0.0)),
            o: Some(static_prop_num(100.0)),
            sk: None,
            sa: None,
        });
        out.push(GraphicElement::Group {
            name: Some(glyph.ch.clone()),
            hidden: false,
            it,
            np: None,
            cix: None,
            bm: None,
            ix: None,
            match_name: None,
        });
    }
    Ok(out)
}

fn scale_transform(size: f64) -> GraphicElement {
    GraphicElement::Transform {
        name: None,
        hidden: false,
        p: Some(static_prop(vec![0.0, 0.0])),
        a: Some(static_prop(vec![0.0, 0.0])),
        s: Some(static_prop(vec![size, size])),
        r: Some(static_prop_num(0.0)),
        o: Some(static_prop_num(100.0)),
        sk: None,
        sa: None,
    }
}

fn static_prop(v: Vec<f64>) -> Property {
    Property::Static(StaticProperty {
        animated: None,
        value: serde_json::Value::Array(v.into_iter().map(serde_json::Value::from).collect()),
        ix: None,
        x: None,
    })
}

fn static_prop_num(v: f64) -> Property {
    Property::Static(StaticProperty {
        animated: None,
        value: serde_json::Value::from(v),
        ix: None,
        x: None,
    })
}

use super::property::StaticProperty;
