// Rewriting generated ids on a live subtree.
//
// The compiler leaves a marker where an id needs a suffix — `--u` where it has
// to be unique per mount, `--c` where it has to be unique per clone of a
// precomp body — because it cannot know how many times a module will be
// mounted or a body cloned. `url(#…)` resolves document-wide, so without the
// suffix a second mount would repaint the first one's gradient.
//
// Every attribute is checked rather than the list of id-bearing ones (`fill`,
// `stroke`, `mask`, `clip-path`, `filter`, `href`, `marker-*`, `style`, …):
// the marker appears only where the compiler put it, so a blind pass is both
// shorter than the list and complete, including presentation attributes moved
// into `style` and any attribute added later.
export function suffixIds(root, mark, sfx) {
  const walk = (el) => {
    for (const a of el.attributes) {
      if (a.value.indexOf(mark) >= 0) a.value = a.value.split(mark).join(sfx);
    }
    for (const c of el.children) walk(c);
  };
  walk(root);
  return root;
}
