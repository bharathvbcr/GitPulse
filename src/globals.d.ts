/**
 * App version injected at build time from package.json (see
 * `scripts/app-version.mjs`, wired in vite.config.ts and vitest.config.ts).
 *
 * Read through `APP_VERSION` in `lib/diagnostics/diagnostics.ts` rather than
 * directly, so a runner that does not define it degrades to "unknown" instead
 * of throwing.
 */
declare const __APP_VERSION__: string;
