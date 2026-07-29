// Highlighting for the artifact viewer.
//
// Formatting is *not* done here. The compiler already emits an unminified form
// — one element and one binding per line, with a provenance header — and it is
// the form `_fixtures/__snapshots__/` is reviewed in. The viewer fetches that
// instead of re-deriving structure from the minified bytes, which a formatter
// has to get right about template literals, regexes and comments to avoid
// corrupting the one thing worth looking at.
//
// What is left is colour, and that is a real grammar's job. Shiki is loaded
// lazily so the page does not pay for it until a row is opened.

import type { HighlighterCore } from 'shiki/core';

export type Lang = 'javascript' | 'json' | 'xml';

/** Guess from the URL, since that is all the caller reliably has. */
export function langOf(url: string): Lang {
  if (url.includes('.json') || url.includes('application/json')) return 'json';
  if (url.includes('.svg') || url.includes('image/svg')) return 'xml';
  return 'javascript';
}

let loading: Promise<HighlighterCore> | null = null;

/**
 * Three grammars and one theme, not the bundled entry.
 *
 * Importing `shiki` wholesale emits a chunk per language — wolfram,
 * emacs-lisp, cpp — for a page that shows JavaScript, JSON and XML. The
 * JavaScript regex engine avoids the oniguruma wasm blob on top of that.
 */
function highlighter(): Promise<HighlighterCore> {
  loading ??= Promise.all([
    import('shiki/core'),
    import('shiki/engine/javascript'),
    import('@shikijs/langs/javascript'),
    import('@shikijs/langs/json'),
    import('@shikijs/langs/xml'),
    import('@shikijs/themes/github-light'),
  ]).then(([core, engine, js, json, xml, theme]) =>
    core.createHighlighterCore({
      themes: [theme.default],
      langs: [js.default, json.default, xml.default],
      engine: engine.createJavaScriptRegexEngine(),
    }),
  );
  return loading;
}

const esc = (s: string) =>
  s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');

/**
 * Highlighted HTML for one artifact.
 *
 * Falls back to escaped plain text if shiki fails to load — an offline static
 * build should still show the source, just without colour.
 */
export async function highlight(src: string, lang: Lang): Promise<string> {
  try {
    const hl = await highlighter();
    return hl.codeToHtml(src, { lang, theme: 'github-light' });
  } catch {
    return `<pre class="plain">${esc(src)}</pre>`;
  }
}
