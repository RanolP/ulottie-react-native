//! Pre-loadable assets — the extraction half of the EXPLAINER's image story.
//!
//! Lottie embeds images as `data:` URIs, which puts their bytes inside the
//! module: they cannot start loading until the module itself has arrived and
//! been parsed. This pass rewrites every *oversized* embedded image into a
//! plain URL the page can fetch concurrently — and, more importantly, preload
//! via `<link rel=preload>` / 103 Early Hints using the manifest it returns.
//! Images at or below the threshold stay inline: a data URI smaller than the
//! request that would replace it is the cheaper delivery.
//!
//! The pass runs on the planned [`Scene`] *strings* — `markup`, `inline` and
//! the template table — after `seal()`. It never touches the wire stream
//! (image layers are pure markup, no binding addresses them), so no re-seal
//! is needed, and because every consumer (document, sprite/symbol, module
//! `M`, codegen `M`) reads those fields, one rewrite covers them all.
//!
//! Robustness rules, each of which leaves the data URI where it was rather
//! than guessing: only `href="…"` attribute values are considered (a data URI
//! in text is not a reference), the MIME type has to map to a known
//! extension, and the payload has to decode. Off by default — see
//! [`crate::AssetOptions`].

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use base64::Engine as _;

use super::Scene;
use crate::AssetOptions;

/// One image pulled out of the markup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedAsset {
    /// Content-hashed filename, `img_<10 hex>.<ext>` — identical payloads
    /// collapse to one file, and the name is stable across recompiles.
    pub name: String,
    /// The MIME type as written in the data URI (`image/png`, …).
    pub mime: String,
    /// The decoded image bytes; what the server writes to `name`.
    pub bytes: Vec<u8>,
}

/// Rewrite the scene's oversized embedded images into URL references.
///
/// Returns the distinct assets the markup now points at, in first-use order.
/// Both `Scene::markup` and `Scene::inline` (and the template table, which
/// ships as `TPL` strings) are rewritten, so the document, the sprite, the
/// interpreter's `M` and the generated module's `M` all agree on the URL.
pub fn extract(scene: &mut Scene, options: &AssetOptions) -> Vec<ExtractedAsset> {
    let mut pass = Pass {
        base: normalize_base(&options.url_base),
        threshold: options.threshold,
        names: HashMap::new(),
        out: Vec::new(),
    };
    scene.markup = pass.rewrite(&scene.markup);
    scene.inline = pass.rewrite(&scene.inline);
    for t in &mut scene.data.tpl {
        *t = pass.rewrite(t);
    }
    pass.out
}

/// `url_base` as used in the markup and manifest: always slash-terminated, so
/// `url_base + name` is a valid relative URL for both `"assets"` and
/// `"assets/"` spellings.
fn normalize_base(url_base: &str) -> String {
    if url_base.ends_with('/') {
        url_base.to_string()
    } else {
        format!("{url_base}/")
    }
}

/// The manifest of extracted assets, as JSON: one
/// `{"url","file","mime","bytes"}` object per asset, `bytes` being the
/// decoded size. A web server turns this into 103 Early Hints or
/// `<link rel=preload>` entries; empty array when nothing was extracted.
pub fn manifest(assets: &[ExtractedAsset], url_base: &str) -> String {
    let base = normalize_base(url_base);
    #[derive(serde::Serialize)]
    struct Entry<'a> {
        url: String,
        file: &'a str,
        mime: &'a str,
        bytes: usize,
    }
    let entries: Vec<Entry> = assets
        .iter()
        .map(|a| Entry {
            url: format!("{base}{}", a.name),
            file: &a.name,
            mime: &a.mime,
            bytes: a.bytes.len(),
        })
        .collect();
    serde_json::to_string_pretty(&entries).unwrap_or_else(|_| "[]".to_string())
}

struct Pass {
    base: String,
    threshold: usize,
    /// Content hash → filename, so the same image referenced twice (markup and
    /// inline are two copies of one tree) yields one file.
    names: HashMap<u64, String>,
    out: Vec<ExtractedAsset>,
}

impl Pass {
    /// Rewrite every oversized `href="data:…"` in `s` into `href="<base><name>"`.
    ///
    /// The scan is anchored on `href="`, which covers `xlink:href="` too — the
    /// attribute *value* runs to the next `"`, so a data URI mentioned
    /// anywhere else in the markup is never touched.
    fn rewrite(&mut self, s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut at = 0;
        while let Some(found) = s[at..].find("href=\"") {
            let uri_start = at + found + "href=\"".len();
            out.push_str(&s[at..uri_start]);
            let Some(uri_end_rel) = s[uri_start..].find('"') else {
                // Unterminated attribute: not our markup, leave the rest.
                out.push_str(&s[uri_start..]);
                return out;
            };
            let uri_end = uri_start + uri_end_rel;
            match self.resolve(&s[uri_start..uri_end]) {
                Some(url) => out.push_str(&url),
                None => out.push_str(&s[uri_start..uri_end]),
            }
            at = uri_end;
        }
        out.push_str(&s[at..]);
        out
    }

    /// A data URI becomes a file reference, or stays as it was.
    fn resolve(&mut self, uri: &str) -> Option<String> {
        let rest = uri.strip_prefix("data:")?;
        let (mime, payload) = rest.split_once(";base64,")?;
        let ext = ext_of(mime)?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(payload)
            .ok()?;
        // Above the threshold means strictly above: "at or below stays
        // inline" is the documented contract.
        if bytes.len() <= self.threshold {
            return None;
        }
        let mut hasher = DefaultHasher::new();
        bytes.hash(&mut hasher);
        let hash = hasher.finish();
        let name = match self.names.get(&hash) {
            Some(name) => name.clone(),
            None => {
                let name = format!("img_{hash:010x}.{ext}");
                self.names.insert(hash, name.clone());
                self.out.push(ExtractedAsset {
                    name: name.clone(),
                    mime: mime.to_string(),
                    bytes,
                });
                name
            }
        };
        Some(format!("{}{name}", self.base))
    }
}

/// The file extension an extracted asset gets. An image type without a
/// mapping is refused extraction (stays inline) rather than written under a
/// guessed extension the server would serve with the wrong Content-Type.
fn ext_of(mime: &str) -> Option<&'static str> {
    match mime {
        "image/png" => Some("png"),
        "image/jpeg" => Some("jpg"),
        "image/gif" => Some("gif"),
        "image/webp" => Some("webp"),
        "image/svg+xml" => Some("svg"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::{Caps, Scene, SceneData};

    // 100 × 'A': decodes to 75 bytes (100 base64 chars), comfortably over any small threshold.
    const BIG: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    const SMALL: &str = "AAAA";

    fn scene_with(markup: &str) -> Scene {
        Scene {
            markup: markup.to_string(),
            inline: markup.to_string(),
            data: SceneData::default(),
            caps: Caps::empty(),
        }
    }

    fn opts(threshold: usize) -> AssetOptions {
        AssetOptions {
            extract: true,
            url_base: "assets/".into(),
            threshold,
        }
    }

    fn img(payload: &str) -> String {
        format!("<image href=\"data:image/png;base64,{payload}\"/>")
    }

    #[test]
    fn above_threshold_is_extracted_and_referenced() {
        let mut s = scene_with(&img(BIG));
        let assets = extract(&mut s, &opts(16));
        assert_eq!(assets.len(), 1);
        assert!(assets[0].name.starts_with("img_"));
        assert!(assets[0].name.ends_with(".png"));
        assert_eq!(assets[0].bytes.len(), 75);
        assert_eq!(s.markup, format!("<image href=\"assets/{}\"/>", assets[0].name));
        assert_eq!(s.inline, s.markup);
    }

    #[test]
    fn at_or_below_threshold_stays_inline() {
        let mut s = scene_with(&img(SMALL));
        let before = s.markup.clone();
        let assets = extract(&mut s, &opts(4));
        assert!(assets.is_empty());
        assert_eq!(s.markup, before);
    }

    #[test]
    fn identical_payloads_dedup_to_one_file() {
        let mut s = scene_with(&format!("{}{}", img(BIG), img(BIG)));
        let assets = extract(&mut s, &opts(16));
        assert_eq!(assets.len(), 1);
        assert_eq!(s.markup.matches("assets/").count(), 2);
    }

    #[test]
    fn unknown_mime_stays_inline() {
        let mut s = scene_with(&format!(
            "<image href=\"data:image/avif;base64,{BIG}\"/>"
        ));
        let before = s.markup.clone();
        assert!(extract(&mut s, &opts(16)).is_empty());
        assert_eq!(s.markup, before);
    }

    #[test]
    fn undecodable_payload_stays_inline() {
        let mut s = scene_with("<image href=\"data:image/png;base64,!!!!\"/>");
        let before = s.markup.clone();
        assert!(extract(&mut s, &opts(0)).is_empty());
        assert_eq!(s.markup, before);
    }

    #[test]
    fn only_href_attributes_are_rewritten() {
        // A data URI as text content between elements must survive verbatim.
        let mut s = scene_with(&format!(
            "<text>data:image/png;base64,{} ok</text>{}",
            BIG,
            img(BIG)
        ));
        let assets = extract(&mut s, &opts(16));
        assert_eq!(assets.len(), 1);
        assert!(s.markup.contains(&format!("data:image/png;base64,{BIG} ok")));
        assert!(s.markup.contains(&format!("assets/{}", assets[0].name)));
    }

    #[test]
    fn templates_are_rewritten_too() {
        let mut s = scene_with("");
        s.data.tpl.push(img(BIG));
        let assets = extract(&mut s, &opts(16));
        assert_eq!(assets.len(), 1);
        assert_eq!(s.data.tpl[0], format!("<image href=\"assets/{}\"/>", assets[0].name));
    }

    #[test]
    fn url_base_gets_a_trailing_slash() {
        let mut s = scene_with(&img(BIG));
        let options = AssetOptions {
            extract: true,
            url_base: "img".into(),
            threshold: 16,
        };
        let assets = extract(&mut s, &options);
        assert_eq!(s.markup, format!("<image href=\"img/{}\"/>", assets[0].name));
    }

    #[test]
    fn the_manifest_lists_every_asset() {
        let mut s = scene_with(&img(BIG));
        let assets = extract(&mut s, &opts(16));
        let m = manifest(&assets, "assets/");
        let v: serde_json::Value = serde_json::from_str(&m).unwrap();
        let e = &v[0];
        assert_eq!(e["file"], assets[0].name);
        assert_eq!(e["url"], format!("assets/{}", assets[0].name));
        assert_eq!(e["mime"], "image/png");
        assert_eq!(e["bytes"], 75);
    }

    #[test]
    fn the_manifest_is_empty_without_assets() {
        assert_eq!(manifest(&[], "assets/"), "[]");
    }
}
