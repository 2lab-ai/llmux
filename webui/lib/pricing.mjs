// Per-model token rates recovered from the daemon's own priced aggregates.
// `usage_stats` rows (hour/day × model) carry tokens and the daemon-computed `cost_usd`, which is linear:
//   cost = in·a + out·b + cache_read·c + cache_creation·d
// Solving those rows by least squares gives the rates the daemon used (overrides included), so
// history rows that carry tokens but no cost can be priced without copying src/pricing.rs.

const COLS = ['tokens_in', 'tokens_out', 'cache_read', 'cache_creation'];
const SCALE = 1e-6; // work in millions of tokens so the normal equations stay well conditioned

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

/** @returns {Map<string, {in:number,out:number,cr:number,cc:number}>} USD per token, per model */
export function solveRates(usageStats) {
  const byModel = new Map();
  for (const u of usageStats || []) {
    if (!u || u.priced === false || !(u.cost_usd > 0) || (u.gran !== 'hour' && u.gran !== 'day')) continue;
    if (!byModel.has(u.model)) byModel.set(u.model, []);
    byModel.get(u.model).push(u);
  }
  const out = new Map();
  for (const [model, rows] of byModel) {
    const live = COLS.filter(c => rows.some(r => (r[c] || 0) > 0));
    if (!live.length || rows.length < live.length) continue;
    const n = live.length;
    const A = Array.from({ length: n }, () => new Array(n).fill(0));
    const b = new Array(n).fill(0);
    for (const r of rows) {
      const x = live.map(c => (r[c] || 0) * SCALE);
      for (let i = 0; i < n; i++) { b[i] += x[i] * r.cost_usd; for (let j = 0; j < n; j++) A[i][j] += x[i] * x[j]; }
    }
    const sol = solve(A, b);
    if (!sol || sol.some(v => !Number.isFinite(v) || v < -1e-6)) continue;
    // Accept only an exact-looking fit: the daemon's cost is linear, so the residual must be tiny.
    let err = 0, tot = 0;
    for (const r of rows) {
      const est = live.reduce((a, c, i) => a + (r[c] || 0) * SCALE * sol[i], 0);
      err += Math.abs(est - r.cost_usd); tot += r.cost_usd;
    }
    if (tot <= 0 || err / tot > 0.02) continue;
    const rate = { in: 0, out: 0, cr: 0, cc: 0 };
    live.forEach((c, i) => { rate[{ tokens_in: 'in', tokens_out: 'out', cache_read: 'cr', cache_creation: 'cc' }[c]] = Math.max(0, sol[i]) * SCALE; });
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

export function priceTokens(model, t, rates, avg) {
  if (!t) return 0;
  const r = rates && rates.get(model);
  if (r) return (t.input || 0) * r.in + (t.output || 0) * r.out + (t.cache_read || 0) * r.cr + (t.cache_creation || 0) * r.cc;
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
