// Per-model token rates recovered from the daemon's own priced aggregates.
// `usage_stats` rows (hour/day × model) carry tokens and the daemon-computed `cost_usd`, which is linear:
//   cost = in·a + out·b + cache_read·c + short writes·d + extended writes·e + web_search_requests·f
// (#278: the extended-TTL writes — Anthropic 1h, OpenAI 12h — are the `cache_creation_extended` part of
// `cache_creation`, the short-TTL writes the rest; `d` and `e` are both absolute write rates and `f` is
// the per-search fee.) Solving those rows by least squares gives the rates the daemon used (overrides
// included), so history rows that carry tokens but no cost can be priced without copying src/pricing.rs.

/** The extended (`ext`) and short (`short`) parts of a row's cache writes — disjoint counts, so a model
 *  whose writes are all one TTL still has independent columns. */
function writes(cc, ext) {
  const total = cc || 0;
  const extended = Math.min(ext || 0, total);
  return { short: total - extended, extended };
}

// One solver column per rate: its key in the rate object, its count on a `usage_stats` row, and the
// unit it is solved in (millions of tokens, thousands of searches) so the normal equations stay well
// conditioned.
const COLS = [
  { key: 'in', get: r => r.tokens_in || 0, scale: 1e-6 },
  { key: 'out', get: r => r.tokens_out || 0, scale: 1e-6 },
  { key: 'cr', get: r => r.cache_read || 0, scale: 1e-6 },
  { key: 'cc', get: r => writes(r.cache_creation, r.cache_creation_extended).short, scale: 1e-6 },
  { key: 'ce', get: r => writes(r.cache_creation, r.cache_creation_extended).extended, scale: 1e-6 },
  { key: 'ws', get: r => r.web_search_requests || 0, scale: 1e-3 },
];

function solve(A, b) {
  const n = b.length;
  const M = A.map((row, i) => [...row, b[i]]);
  for (let c = 0; c < n; c++) {
    let p = c;
    for (let r = c + 1; r < n; r++) if (Math.abs(M[r][c]) > Math.abs(M[p][c])) p = r;
    if (Math.abs(M[p][c]) < 1e-12) return null;
    [M[c], M[p]] = [M[p], M[c]];
    for (let r = 0; r < n; r++) {
      if (r === c) continue;
      const f = M[r][c] / M[c][c];
      for (let k = c; k <= n; k++) M[r][k] -= f * M[c][k];
    }
  }
  return M.map((row, i) => row[n] / row[i]);
}

/** @returns {Map<string, {in:number,out:number,cr:number,cc:number,ce:number,ws:number}>} USD per token
 *  (`cc`: short-TTL write, `ce`: extended-TTL write; `ws`: USD per web search), per model. A column no
 *  solved row has is 0, except the write rates: an unobserved extended rate is the short one (the
 *  daemon's rule for a row with no extended rate), and an unobserved short rate the extended one (a
 *  model whose recent writes were all extended; its unsplit history writes get that rate). */
export function solveRates(usageStats) {
  const byModel = new Map();
  for (const u of usageStats || []) {
    if (!u || u.priced === false || !(u.cost_usd > 0) || (u.gran !== 'hour' && u.gran !== 'day')) continue;
    if (!byModel.has(u.model)) byModel.set(u.model, []);
    byModel.get(u.model).push(u);
  }
  const out = new Map();
  for (const [model, rows] of byModel) {
    const live = COLS.filter(c => rows.some(r => c.get(r) > 0));
    if (!live.length || rows.length < live.length) continue;
    const n = live.length;
    const A = Array.from({ length: n }, () => new Array(n).fill(0));
    const b = new Array(n).fill(0);
    for (const r of rows) {
      const x = live.map(c => c.get(r) * c.scale);
      for (let i = 0; i < n; i++) { b[i] += x[i] * r.cost_usd; for (let j = 0; j < n; j++) A[i][j] += x[i] * x[j]; }
    }
    const sol = solve(A, b);
    // Every rate is absolute, so a negative one is a bad fit, never a discount.
    if (!sol || sol.some(v => !Number.isFinite(v) || v < -1e-6)) continue;
    // Accept only an exact-looking fit: the daemon's cost is linear, so the residual must be tiny.
    let err = 0, tot = 0;
    for (const r of rows) {
      const est = live.reduce((a, c, i) => a + c.get(r) * c.scale * sol[i], 0);
      err += Math.abs(est - r.cost_usd); tot += r.cost_usd;
    }
    if (tot <= 0 || err / tot > 0.02) continue;
    const rate = { in: 0, out: 0, cr: 0, cc: 0, ce: 0, ws: 0 };
    live.forEach((c, i) => { rate[c.key] = Math.max(0, sol[i]) * c.scale; });
    const seen = key => live.some(c => c.key === key);
    if (!seen('ce')) rate.ce = rate.cc;
    else if (!seen('cc')) rate.cc = rate.ce;
    out.set(model, rate);
  }
  return out;
}

/** Average cost per request per model — the fallback when a model's rates cannot be solved. A row's
 *  `sub_cost_usd` (#278: advisor / compaction / billed fallback work other requests ran on this
 *  model) is not any of its requests' cost, so it is left out of the average. */
export function avgPerRequest(modelUsage) {
  const m = new Map();
  for (const u of modelUsage || []) {
    const own = (u.cost_usd || 0) - (u.sub_cost_usd || 0);
    if (u.requests && own > 0) m.set(u.model, own / u.requests);
  }
  return m;
}

/** Counters (activity `tokens` shape) priced at solved rates `r`: the short-TTL writes at `cc`, the
 *  extended ones at `ce` (absent → `cc`, as the daemon prices a row with no extended rate). */
export function priceCounts(t, r) {
  const w = writes(t.cache_creation, t.cache_creation_extended);
  return (t.input || 0) * r.in + (t.output || 0) * r.out + (t.cache_read || 0) * r.cr + w.short * r.cc
    + w.extended * (r.ce ?? r.cc) + (t.web_search_requests || 0) * (r.ws || 0);
}

export function priceTokens(model, t, rates, avg) {
  if (!t) return 0;
  const r = rates && rates.get(model);
  if (r) return priceCounts(t, r);
  return (avg && avg.get(model)) || 0;
}

/** Cost of one activity row: the daemon's own number when it has one (it already includes the
 *  row's sub-usages), else priced from tokens — the request's own at its model's rates plus each
 *  sub-usage (#278: advisor / compaction / billed fallback attempt) at its own model's rates. A
 *  sub-usage on a model with no solved rate adds nothing (a per-request average would overcount). */
export function priceRow(row, rates, avg) {
  if (row.cost_usd != null) return row.cost_usd;
  return (row.sub_usage || []).reduce((sum, s) => {
    const model = s.model || row.model;
    return sum + (rates && rates.get(model) ? priceTokens(model, s.tokens, rates, null) : 0);
  }, priceTokens(row.model, row.tokens, rates, avg));
}

/** One row's token classes with its sub-usages added — what a per-request or per-session TOTAL
 *  token figure counts. The row's own `tokens` (its top-level columns) stay as they are. */
export function rowTokens(row) {
  const out = { input: 0, output: 0, cache_read: 0, cache_creation: 0 };
  for (const t of [row.tokens, ...(row.sub_usage || []).map(s => s.tokens)]) {
    if (!t) continue;
    for (const k of Object.keys(out)) out[k] += t[k] || 0;
  }
  return out;
}
