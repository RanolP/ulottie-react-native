//! Which layer a track matte takes out of the picture, and which it does not.
//!
//! `td` and `tp` answer two different questions. `td` says "this layer is a
//! matte, never draw it"; `tp` says "the layer I am masked by is that one". A
//! layer can be named by someone's `tp` without carrying `td` — After Effects'
//! newer track-matte model does exactly that — and then it is drawn *and* used
//! as a matte, which no single subtree can be.
//!
//! Getting that wrong is expensive and silent: hiding every matte source took a
//! whole map illustration out of one production animation, because its map
//! layer was masked by the `td` layer above it and was itself the matte for two
//! ripples below.

use ulottie_compiler::compile_document;

/// `[matte, masked]` where the matte is named by `tp` and carries no `td`.
///
/// Layer 1 is a red square. Layer 2 is a blue square that names layer 1 as its
/// alpha matte. Both are 100×100 at the origin so the picture is unambiguous.
fn animation(td: bool) -> String {
    let td_field = if td { r#""td":1,"# } else { "" };
    format!(
        r#"{{"v":"5.7.0","fr":30,"ip":0,"op":30,"w":200,"h":200,"assets":[],"layers":[
      {{"ind":1,"ty":4,"nm":"matte","sr":1,{td_field}
       "ks":{{"o":{{"a":0,"k":100}},"r":{{"a":0,"k":0}},"p":{{"a":0,"k":[0,0,0]}},
              "a":{{"a":0,"k":[0,0,0]}},"s":{{"a":0,"k":[100,100,100]}}}},
       "ao":0,"ip":0,"op":30,"st":0,"bm":0,
       "shapes":[{{"ty":"gr","it":[
         {{"ty":"rc","s":{{"a":0,"k":[100,100]}},"p":{{"a":0,"k":[50,50]}},"r":{{"a":0,"k":0}}}},
         {{"ty":"fl","c":{{"a":0,"k":[1,0,0,1]}},"o":{{"a":0,"k":100}}}},
         {{"ty":"tr","p":{{"a":0,"k":[0,0]}},"a":{{"a":0,"k":[0,0]}},
           "s":{{"a":0,"k":[100,100]}},"r":{{"a":0,"k":0}},"o":{{"a":0,"k":100}}}}
       ]}}]}},
      {{"ind":2,"ty":4,"nm":"masked","sr":1,"tt":1,"tp":1,
       "ks":{{"o":{{"a":0,"k":100}},"r":{{"a":0,"k":0}},"p":{{"a":0,"k":[0,0,0]}},
              "a":{{"a":0,"k":[0,0,0]}},"s":{{"a":0,"k":[100,100,100]}}}},
       "ao":0,"ip":0,"op":30,"st":0,"bm":0,
       "shapes":[{{"ty":"gr","it":[
         {{"ty":"rc","s":{{"a":0,"k":[100,100]}},"p":{{"a":0,"k":[100,100]}},"r":{{"a":0,"k":0}}}},
         {{"ty":"fl","c":{{"a":0,"k":[0,0,1,1]}},"o":{{"a":0,"k":100}}}},
         {{"ty":"tr","p":{{"a":0,"k":[0,0]}},"a":{{"a":0,"k":[0,0]}},
           "s":{{"a":0,"k":[100,100]}},"r":{{"a":0,"k":0}},"o":{{"a":0,"k":100}}}}
       ]}}]}}
    ]}}"#
    )
}

/// A matte source that carries `td` is out of the picture: it moves bodily into
/// the `<mask>`, which is where the only copy of it belongs.
#[test]
fn a_td_matte_source_is_not_drawn() {
    let doc = compile_document(&animation(true)).unwrap();
    let (defs, body) = split_defs(&doc);
    assert!(defs.contains("#f00"), "the matte belongs in <defs>:\n{doc}");
    assert!(
        !body.contains("#f00"),
        "a `td` layer is never in the picture:\n{doc}"
    );
    assert!(
        !doc.contains("<use"),
        "nothing to reference — the mask owns the subtree:\n{doc}"
    );
}

/// A matte source named only by `tp` is drawn *as well as* matting. An element
/// can be in one place in a document, so the mask gets a `<use>` pointing at
/// the layer rather than the layer itself.
#[test]
fn a_matte_source_without_td_is_still_drawn() {
    let doc = compile_document(&animation(false)).unwrap();
    let (defs, body) = split_defs(&doc);
    assert!(
        body.contains("#f00"),
        "a layer merely named by `tp` stays in the picture:\n{doc}"
    );
    assert!(
        defs.contains("<use"),
        "the mask reaches it by reference:\n{doc}"
    );

    // …and the reference resolves. An id that does not match is a mask over
    // nothing, which silently deletes the layer it was meant to shape — the
    // exact failure lottie-web ships for this input.
    let href = between(&defs, "href=\"#", "\"").expect("the <use> carries an href");
    assert!(
        body.contains(&format!("id=\"{href}\"")),
        "`<use href=\"#{href}\">` must resolve to an element in the picture:\n{doc}"
    );
}

/// The document is `<body…><defs>…</defs></svg>`: definitions are emitted last
/// so the visible tree's element indices stay stable.
fn split_defs(doc: &str) -> (String, String) {
    match doc.find("<defs>") {
        Some(i) => (doc[i..].to_string(), doc[..i].to_string()),
        None => (String::new(), doc.to_string()),
    }
}

fn between<'a>(s: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = s.find(open)? + open.len();
    let end = s[start..].find(close)? + start;
    Some(&s[start..end])
}
