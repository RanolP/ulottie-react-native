// Expanding factored-out subtrees.
//
// When a document is too large to inline whole, repeated subtrees ship once in
// `D.m` and each occurrence is a `<g data-t="n"/>` placeholder. Expansion has
// to run before elements are indexed: the compiler assigned document-order
// indices over the *expanded* tree, and replacing a placeholder with the
// template's root keeps every one of them correct.

import { suffixIds } from './ids.js';

let clone = 0;

/**
 * Give one clone its own copies of the ids its body defines. `url(#…)`
 * resolves document-wide, so without this every clone of a precomp would point
 * at the first one's gradient or mask.
 */
function scopeIds(root) {
  return suffixIds(root, '--c', '-c' + clone++);
}

export function expand(svg, tpl) {
  const box = document.createElement('div');
  const parse = (m) => {
    box.innerHTML = '<svg>' + m + '</svg>';
    return box.firstChild.firstElementChild;
  };
  const nodes = tpl.map(parse);
  // A precomp body can hold uses of other precomps, so expansion repeats until
  // nothing is left. Each pass snapshots the list first — replaceWith mutates
  // the tree underneath a live NodeList.
  for (;;) {
    const holes = [...svg.querySelectorAll('g[data-t]')];
    if (!holes.length) return;
    for (const el of holes) {
      el.replaceWith(scopeIds(nodes[+el.getAttribute('data-t')].cloneNode(true)));
    }
  }
}
