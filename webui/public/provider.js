/* Which provider a model belongs to — the mark drawn left of every model name.
 * Returns an @2lab.ai/ui model-mark vendor: 'claude' | 'openai' | 'grok' | 'gemini', or 'model' (any other).
 * The model name decides first, because the mark sits beside that name; the llmux backend group
 * (`group` on activity rows: claude / codex / grok / openrouter / gemini) names the ones the name
 * does not, such as an OpenRouter slug of an unknown maker. Loaded as a browser script after core.js
 * (X.providerOf) and by the unit tests through require(). */
(function (root) {
  'use strict';
  const BY_GROUP = { claude: 'claude', codex: 'openai', grok: 'grok', gemini: 'gemini' };
  const BY_NAME = [
    // OpenRouter slugs carry the maker: anthropic/…, openai/…, x-ai/…, google/…
    [/^anthropic\//, 'claude'], [/^openai\//, 'openai'], [/^x-?ai\//, 'grok'], [/^google\//, 'gemini'],
    [/claude|opus|sonnet|haiku|fable/, 'claude'],
    // llmux's codex family aliases: sol / terra / luna / astra (gpt-6-astra, gpt-6.1-sol, …)
    [/^gpt|^o\d|codex|openai|chatgpt|^(sol|terra|luna|astra)\b/, 'openai'],
    [/grok|x-?ai/, 'grok'],
    // gemini rows and the fixed `argon` alias; the `neon` / `carbon` / `hydrogen` aliases (2026-11-21)
    // are anchored like the codex ones, since the bare words also occur in other slugs
    [/gemini|argon|^(neon|carbon|hydrogen)\b/, 'gemini'],
  ];
  const NAMES = { claude: 'Anthropic', openai: 'OpenAI', grok: 'xAI', gemini: 'Google', model: '' };

  function providerOf(model, group) {
    const m = String(model || '').trim().toLowerCase().replace(/\[1m\]$/, '');
    if (m) for (const [re, v] of BY_NAME) if (re.test(m)) return v;
    return BY_GROUP[String(group || '').toLowerCase()] || 'model';
  }
  const providerName = (v) => NAMES[v] || '';

  if (typeof module === 'object' && module.exports) module.exports = { providerOf, providerName };
  else if (root.X) { root.X.providerOf = providerOf; root.X.providerName = providerName; }
})(typeof window !== 'undefined' ? window : globalThis);
