// Sourcing the initial DOM from an external SVG sprite.
//
// In extracted mode the module carries only the outer `<svg>` shell; the
// elements live in a `<symbol>` in a sprite file the page inlines or preloads.
// The markup is then cached — and shared between animations — independently of
// the JS, and the module stays small enough to be worth inlining itself.
//
// The symbol's *children are cloned in*, not referenced with `<use>`. A `<use>`
// instance tree is a closed shadow root: `querySelectorAll('*')` on it returns
// the `<use>` element and nothing else, so a player could never reach the
// `<rect>` it has to animate. Cloning reproduces exactly the document-order
// element sequence the compiler indexed against.

import { suffixIds } from './ids.js';

/**
 * `(svg, suffix) => void` — fill a freshly built `<svg>` from the sprite.
 *
 * Synchronous, so `init()` stays synchronous: the sprite has to already be in
 * the document. That is the point of the mode — it ships with the HTML, or is
 * fetched and injected once for every animation on the page.
 */
export function fromSprite(id) {
  return (svg, sfx) => {
    const sym = document.getElementById(id);
    if (!sym) {
      throw new Error(
        "ulottie: sprite symbol '" +
          id +
          "' is not in the document — inline the sprite before init()",
      );
    }
    for (const c of sym.children) svg.appendChild(c.cloneNode(true));
    if (sfx) suffixIds(svg, '--u', sfx);
  };
}
