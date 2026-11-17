// A one-chunk door into @2lab.ai/ui's model marks (the provider drawings left of model names).
//
// The library exports them only from its root index.js, and importing that index loads every
// component (~80 modules, 2.3 MB) for five small SVG strings. The marks live in one chunk whose
// file name carries a build hash and whose exports are renamed letters, so neither can be named
// here. This reads both from the index's own import and export lines and re-exports the chunk
// under the public names. When the index looks different (another bundler output), it falls back
// to re-exporting from the index — slower, still correct.

const WANT = ['modelMarkSvg', 'MODEL_MAKER_COLOR'];

export function modelMarkShim(indexSource) {
  const fallback = `export { ${WANT.join(', ')} } from './index.js';\n`;
  const src = String(indexSource || '');
  const imp = /import\s*\{([^}]*)\}\s*from\s*["']\.\/(model-mark-[\w-]+\.js)["']/.exec(src);
  if (!imp) return fallback;
  // local name in index.js → export name inside the chunk ("a as a8" → a8: a, "M" → M: M)
  const orig = new Map(imp[1].split(',').map(s => s.trim()).filter(Boolean).map(s => {
    const [from, as] = s.split(/\s+as\s+/);
    return [(as || from).trim(), from.trim()];
  }));
  const names = [];
  for (const pub of WANT) {
    const exp = new RegExp(`(?:^|[\\s{,])([\\w$]+)\\s+as\\s+${pub}\\b`).exec(src);
    const local = exp ? exp[1] : (new RegExp(`[\\s{,]${pub}\\s*[,}]`).test(src) ? pub : null);
    const inner = local && orig.get(local);
    if (!inner) return fallback;
    names.push(inner === pub ? pub : `${inner} as ${pub}`);
  }
  return `export { ${names.join(', ')} } from './${imp[2]}';\n`;
}
