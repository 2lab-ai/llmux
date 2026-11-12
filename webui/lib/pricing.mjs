// Per-model token rates recovered from the daemon's own priced aggregates.
// `usage_stats` rows (hour/day × model) carry tokens and the daemon-computed `cost_usd`, which is linear:
//   cost = in·a + out·b + cache_read·c + short writes·d + extended writes·e + web_search_requests·f
// (#278: the extended-TTL writes — Anthropic 1h, OpenAI 12h — are the `cache_creation_extended` part of
// `cache_creation`, the short-TTL writes the rest; `d` and `e` are both absolute write rates and `f` is
// the per-search fee.) Solving those rows by least squares gives the rates the daemon used (overrides
// included), so history rows that carry tokens but no cost can be priced without copying src/pricing.rs.
// Price classes (#278): a row's `price_classes` shares (long context, Fast, US-only inference, dated
// rates, an unbilled refusal) carry their own tokens and cost. The standard remainder (row minus shares) solves under the
// model's key and each priced share under `model|<class>`; a history row (or sub-usage) is priced with
// the rates of its own `price_class`, and an unbilled refusal costs nothing.

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

/** Stable key of a price class: '' for the standard class (absent / `{}`). */
export function classKey(c) {
  if (!c) return '';
  return [c.long_context && 'long_context', c.fast && 'fast', c.us_inference && 'us_inference', c.rates_from && `rates_from=${c.rates_from}`, c.unbilled && 'unbilled']
    .filter(Boolean).join(',');
}

const rateKey = (model, c) => { const k = classKey(c); return k ? `${model}|${k}` : model; };

// A share's `tokens` (activity shape) under the usage-row column names.
const SHARE_FIELD = { tokens_in: 'input', tokens_out: 'output', cache_read: 'cache_read', cache_creation: 'cache_creation', cache_creation_extended: 'cache_creation_extended', web_search_requests: 'web_search_requests' };

/** One usage row as solver samples (usage-row shape plus a `key`): its standard remainder under the
 *  model, each priced share under its class. An unbilled share costs 0 and is not solved. */
function samples(u) {
  const all = (u.price_classes || []).filter(s => s && s.class);
  const std = { key: u.model, cost_usd: (u.cost_usd || 0) - all.reduce((a, s) => a + (s.cost_usd || 0), 0) };
  for (const [f, t] of Object.entries(SHARE_FIELD)) std[f] = (u[f] || 0) - all.reduce((a, s) => a + ((s.tokens || {})[t] || 0), 0);
  const out = [std];
  for (const s of all) {
    if (s.class.unbilled) continue;
    const x = { key: rateKey(u.model, s.class), cost_usd: s.cost_usd || 0 };
    for (const [f, t] of Object.entries(SHARE_FIELD)) x[f] = (s.tokens || {})[t] || 0;
    out.push(x);
  }
  return out;
}

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
    if (!u || u.priced === false || (u.gran !== 'hour' && u.gran !== 'day')) continue;
    // A row with price classes comes from a daemon that prices every row, so its 0 is a real price
    // (an explicitly free class); an older row's 0 may mean "not priced", so it is not solved.
    const classed = (u.price_classes || []).length > 0;
    if (!classed && !(u.cost_usd > 0)) continue;
    for (const x of samples(u)) {
      if (classed ? !(x.cost_usd >= 0) || !COLS.some(c => c.get(x) > 0) : !(x.cost_usd > 0)) continue;
      if (!byModel.has(x.key)) byModel.set(x.key, []);
      byModel.get(x.key).push(x);
    }
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
    // A key whose every sample cost 0 is free (all its rates 0), not unsolvable.
    if (tot <= 0 ? err > 1e-12 : err / tot > 0.02) continue;
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

/** The solved rates for `model` in price class `cls`: the class's own, else the model's standard
 *  rates (an estimate below the real class price), else none. */
export function ratesFor(rates, model, cls) {
  if (!rates) return null;
  return rates.get(rateKey(model, cls)) || rates.get(model) || null;
}

export function priceTokens(model, t, rates, avg, cls) {
  if (!t || (cls && cls.unbilled)) return 0;
  const r = ratesFor(rates, model, cls);
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
    return sum + (ratesFor(rates, model, s.price_class) ? priceTokens(model, s.tokens, rates, null, s.price_class) : 0);
  }, priceTokens(row.model, row.tokens, rates, avg, row.price_class));
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
