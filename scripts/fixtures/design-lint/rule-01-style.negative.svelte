<p class="note">a</p>
<style>
  .note { font-size: var(--text-meta); }
  .display { font-size: var(--text-display); }
  .same { font-size: inherit; }
  .calc { font-size: calc(var(--text-xs) * 1.1); }
  .chip { @apply text-meta font-medium; }
  /* The size in a font shorthand is a token. */
  .shorthand { font: 600 var(--text-meta)/1.8 var(--font-ui); }
  /* A literal fallback inside a --text-* token is allowed: the token contract
     test keeps every step defined in app.css, so the fallback is unreachable
     and only keeps an isolated page (a harness without app.css) legible. */
  .fallback { font-size: var(--text-micro, 0.625rem); }
  .fallback-shorthand { font: var(--text-xs, 0.75rem)/1.5 system-ui; }
  .fallback-weighted { font: 500 var(--text-sm, 0.8125rem) / 1.4 var(--font-ui); }
  /* A literal line-height is not a size, and a system font or keyword has none. */
  .line-height { font: var(--text-xs)/1.8rem system-ui; }
  .unknown-size { font: 500 var(--fs)/1.8rem system-ui; }
  .system { font: menu; }
  .reset { font: inherit; }
  /* font-size: 11px; font-size: 0.75rem; named in a comment is not a declaration */
</style>
