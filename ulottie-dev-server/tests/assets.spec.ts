// End-to-end proof of the extraction + SSR story:
//
//   1. the CLI, asked to extract (`--assets`, threshold 0 so the tiny fixture
//      image qualifies), emits a module, a sprite and a document that all
//      reference the extracted file instead of a `data:` URI;
//   2. the file and its manifest exist and are served with the right type;
//   3. the `--no-markup` module mounted onto markup parsed from the baked
//      document yields a working animation — the milestone's core claim,
//      because that pair is exactly what an SSR response hands the client:
//      the document in the HTML, and a script that carries no second copy.
//
// `image_embedded` is the fixture: one embedded 16×16 PNG under the default
// 4096-byte threshold, hence `--asset-threshold 0` — above 0 strictly, so it
// extracts. Its layer rotates 0→30 over 50 frames, so a mid-frame check can
// tell a live binding from a leftover baked attribute.
//
// Compilation runs on the host (`ssrCompile`, see vite.config.ts) because only
// the host can shell out to the release binary; the artifacts are written into
// the shared `.output/` dir, so the browser imports the module from
// `/.output/ssr-e2e.js` — whose `./runtime/**` imports then resolve against
// the dev server's runtime route.

import { commands } from 'vitest/browser';
import { describe, expect, test } from 'vitest';

// By relative path, not the `#wasm` alias: vitest resolves test-file imports
// Node-style, where a `#`-specifier is a package.json "imports" entry and
// there isn't one — the alias only lives in Vite's browser pipeline, which is
// how the demo worker gets away with it.
import init, { compileRequest } from '../demo/src/generated/wasm/ulottie_compiler.js';

import { ArchivedCompileResponse } from '../demo/src/generated/bindings.ts';

await init();

/** One element as a comparable shape — see visual.spec.ts for the rationale:
 *  attribute order and serialisation are not meaningful, per-mount ids are. */
const shape = (el: Element) => {
  const attrs: Record<string, string> = {};
  for (const at of el.attributes) {
    if (at.name === 'id' || at.name === 'style' || at.name === 'xmlns') continue;
    if (at.value.includes('url(#') || at.value.includes('--u')) continue;
    attrs[at.name] = at.value;
  }
  attrs['@display'] = (el as SVGElement).style?.display ?? '';
  return { tag: el.tagName, attrs };
};

const compare = (a: ReturnType<typeof shape>[], b: ReturnType<typeof shape>[]) => {
  const off: string[] = [];
  for (const [i, x] of a.entries()) {
    const y = b[i];
    if (!y) { off.push(`[${i}] <${x.tag}> missing`); continue; }
    if (x.tag !== y.tag) { off.push(`[${i}] ${x.tag} vs ${y.tag}`); continue; }
    for (const k of new Set([...Object.keys(x.attrs), ...Object.keys(y.attrs)])) {
      if (x.attrs[k] !== y.attrs[k]) off.push(`[${i}] <${x.tag}> ${k}: ${x.attrs[k]} vs ${y.attrs[k]}`);
    }
  }
  return off;
};

describe('asset extraction, end to end', () => {
  test('the module, sprite and document reference the extracted file', { timeout: 30_000 }, async () => {
    const out = await commands.ssrCompile('image_embedded');

    // The manifest says what was extracted, and the host found the files.
    expect(out.assetFiles.length).toBeGreaterThanOrEqual(1);
    expect(out.manifest.length).toBe(out.assetFiles.length);
    const entry = out.manifest[0];
    expect(entry.mime).toBe('image/png');
    expect(entry.file).toMatch(/^img_[0-9a-f]+\.png$/);
    expect(entry.url).toBe(`assets/${entry.file}`);
    expect(entry.bytes).toBeGreaterThan(0);

    // Every artifact that ships markup points at the file, and none of them
    // still carries the image as a data URI (the fixture has exactly one).
    for (const [what, text] of [
      ['module', out.module],
      ['sprite', out.sprite],
      ['document', out.document],
    ] as const) {
      expect(text, `${what} references the extracted asset`).toContain(entry.url);
      expect(text, `${what} still embeds a data URI`).not.toContain('data:image');
    }
    // The hydration module carries no markup, so it references no image either
    // way — the document it adopts already does.
    expect(out.hydrate, 'the hydration module carries markup').not.toContain(entry.url);
    expect(out.hydrate, 'the hydration module embeds a data URI').not.toContain('data:image');

    // Both are served: the file with its content type, the manifest as JSON.
    const asset = await fetch(`/.output/assets/${entry.file}`);
    expect(asset.status).toBe(200);
    expect(asset.headers.get('content-type')).toContain('image/png');
    expect((await asset.arrayBuffer()).byteLength).toBe(entry.bytes);
    const manifest = await fetch('/.output/assets/manifest.json');
    expect(manifest.status).toBe(200);
    expect(manifest.headers.get('content-type')).toContain('application/json');
    expect(await manifest.json()).toEqual(out.manifest);
  });

  // The milestone's core claim: the baked document is not just a pretty first
  // frame — it is the DOM the `--no-markup` module adopts and then drives.
  // Same shape of check as the hydration suite in visual.spec.ts, but the
  // served markup comes from the CLI's `--document` output whose image is an
  // extracted URL, not an inline data URI, and the script is the real SSR
  // module: self-contained, with no copy of the document inside it.
  test('the baked document hydrates into a working animation', { timeout: 30_000 }, async () => {
    const out = await commands.ssrCompile('image_embedded');

    document.body.innerHTML = '';
    // The document's image href is relative (`assets/<file>`); resolve it
    // against the directory the assets are actually served from, the way a
    // page deploying these artifacts would place them next to each other.
    const base = document.createElement('base');
    base.href = new URL('/.output/', location.href).href;
    document.head.appendChild(base);

    // Stand in for the server's response: the baked document, as parsed.
    const served = document.createElement('div');
    served.innerHTML = out.document;
    document.body.appendChild(served);
    const before = served.querySelector('svg')!.querySelectorAll('*').length;

    const fresh = document.createElement('div');
    document.body.appendChild(fresh);

    const mod = await import(/* @vite-ignore */ `/.output/ssr-e2e.js?t=${Date.now()}`);
    const hydrator = await import(/* @vite-ignore */ `${out.hydrateUrl}?t=${Date.now()}`);
    expect(hydrator.markup, 'a hydration module exports no markup').toBeUndefined();
    // No `hydrate` flag: a module with no markup of its own can only adopt.
    const a = hydrator.init(served, { autoplay: false, loop: false });
    const b = mod.init(fresh, { autoplay: false, loop: false });
    const transforms = (host: HTMLElement) =>
      [...host.querySelector('svg')!.querySelectorAll('*')]
        .map((el) => el.getAttribute('transform') ?? '')
        .join('|');
    try {
      // Adopted, not re-rendered: the same nodes are still there.
      expect(served.querySelector('svg')!.querySelectorAll('*').length).toBe(before);

      // Mid-animation (the layer rotates 0→30 over 50 frames): an element
      // indexed wrongly or a binding that never adopted would still look right
      // at the frame the markup was baked at.
      const baked = transforms(served);
      const mid = Math.floor(b.totalFrames / 2);
      a.goToFrame(mid);
      b.goToFrame(mid);
      const shot = (host: HTMLElement) =>
        [...host.querySelector('svg')!.querySelectorAll('*')].map(shape);
      expect(compare(shot(served), shot(fresh)), 'hydrated').toEqual([]);

      // And the frame actually moved — the rotation is a live binding on the
      // adopted tree, not a leftover baked attribute.
      expect(transforms(served), 'the hydrated layer animates').not.toBe(baked);
    } finally {
      a.destroy();
      b.destroy();
      base.remove();
    }
  });
});

// The dev server's half of the story: its compiles extract with
// `url_base = /.output/<id>/assets/`, and that route serves what the markup
// asks for. `image_embedded`'s image is 84 bytes — under the 4096 default — so
// the over-threshold case is exercised through an upload whose image is
// generated large (canvas noise compresses poorly, clearing the threshold
// comfortably).
describe('the dev server extracts and serves assets', () => {
  test('a fixture under the threshold extracts nothing, but the manifest is served', {
    timeout: 30_000,
  }, async () => {
    // Requesting the module drives the lazy compile that writes the assets.
    expect((await fetch('/.output/image_embedded.js')).ok).toBe(true);
    const res = await fetch('/.output/image_embedded/assets/manifest.json');
    expect(res.ok).toBe(true);
    // The image stays inline: a data URI smaller than the request that would
    // replace it is the cheaper delivery.
    expect(await res.json()).toEqual([]);
    // …and the module keeps it inline.
    const js = await (await fetch('/.output/image_embedded.js')).text();
    expect(js).toContain('data:image/png;base64,');
  });

  test('an upload over the threshold gets extracted, served, and referenced', {
    timeout: 60_000,
  }, async () => {
    const canvas = document.createElement('canvas');
    canvas.width = 256;
    canvas.height = 256;
    const ctx = canvas.getContext('2d')!;
    for (let i = 0; i < 400; i++) {
      ctx.fillStyle = `rgb(${(Math.random() * 256) | 0},${(Math.random() * 256) | 0},${
        (Math.random() * 256) | 0
      })`;
      ctx.fillRect(Math.random() * 256, Math.random() * 256, 12, 12);
    }
    const png = canvas.toDataURL('image/png');
    expect(png.length).toBeGreaterThan(4096);

    const src = await (await fetch('/.output/image_embedded.json')).text();
    const json = JSON.parse(src);
    json.assets[0].p = png;

    const res = await fetch('/compile', { method: 'POST', body: JSON.stringify(json) });
    expect(res.ok).toBe(true);
    const info = ArchivedCompileResponse.decode(new Uint8Array(await res.arrayBuffer()));

    const manifestRes = await fetch(`/.output/${info.id}/assets/manifest.json`);
    expect(manifestRes.ok).toBe(true);
    const manifest = (await manifestRes.json()) as {
      url: string;
      file: string;
      mime: string;
      bytes: number;
    }[];
    expect(manifest.length).toBe(1);
    const entry = manifest[0];
    expect(entry.mime).toBe('image/png');
    expect(entry.url).toBe(`/.output/${info.id}/assets/${entry.file}`);

    // The shipped module points at the route, and no longer carries the bytes.
    const js = await (await fetch(info.js_url)).text();
    expect(js).toContain(entry.url);
    expect(js).not.toContain('data:image/png');

    // The route serves the file with its content type.
    const asset = await fetch(entry.url);
    expect(asset.status).toBe(200);
    expect(asset.headers.get('content-type')).toContain('image/png');
    expect((await asset.arrayBuffer()).byteLength).toBe(entry.bytes);
  });
});

// The in-browser wasm build is the same crate with the same feature surface —
// no capability may exist only behind the native build. Extraction is the one
// that needs help in a page (nowhere to write files), so it is the one proved
// here: the wasm compile extracts, references `assets/<name>` exactly as the
// CLI does, and hands the bytes back for the page to mint Blob URLs from.
describe('the wasm build extracts too', () => {
  test('an over-threshold image comes out as bytes plus a URL reference', { timeout: 60_000 },
    async () => {
      // Same source as the dev-server upload test: canvas noise compresses
      // poorly, clearing the 4096-byte default threshold comfortably.
      const canvas = document.createElement('canvas');
      canvas.width = 256;
      canvas.height = 256;
      const ctx = canvas.getContext('2d')!;
      for (let i = 0; i < 400; i++) {
        ctx.fillStyle = `rgb(${(Math.random() * 256) | 0},${(Math.random() * 256) | 0},${
          (Math.random() * 256) | 0
        })`;
        ctx.fillRect(Math.random() * 256, Math.random() * 256, 12, 12);
      }
      const src = await (await fetch('/.output/image_embedded.json')).text();
      const json = JSON.parse(src);
      json.assets[0].p = canvas.toDataURL('image/png');

      const r = compileRequest(JSON.stringify(json));
      try {
        expect(r.assetCount).toBe(1);
        expect(r.assetMime(0)).toBe('image/png');
        const file = r.assetName(0)!;
        expect(file).toMatch(/^img_[0-9a-f]+\.png$/);

        // The markup references the file the way the CLI's output does — the
        // worker's Blob-URL rewrite is a plain string replace over exactly
        // this spelling — and no longer carries the bytes.
        const module = new TextDecoder().decode(r.compiledEmbedded);
        expect(module).toContain(`assets/${file}`);
        expect(module).not.toContain('data:image/png');
        expect(new TextDecoder().decode(r.spriteSvg)).toContain(`assets/${file}`);

        // The bytes are the file: a real PNG.
        const bytes = r.assetBytes(0)!;
        expect(bytes.length).toBeGreaterThan(4096);
        expect([...bytes.slice(0, 8)]).toEqual([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
      } finally {
        r.free();
      }
    });
});
