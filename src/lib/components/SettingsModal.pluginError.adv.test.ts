import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "SettingsModal.svelte"),
  "utf8",
);

/**
 * W6-L5 adversarial review. BREAK: `mcp_info` now writes the first template
 * problem into `plugin_error` only when `plugin_found` is true (the resolver's
 * own error is empty then), but Settings renders `plugin_error` only in the
 * `{:else}` of `{#if mcpInfo.plugin_found}`. The new error is never shown: the
 * blank "copy this config" the change set out to explain is still unexplained.
 */
describe("Settings plugin line", () => {
  it("BREAK: shows plugin_error when the plugin was found but its templates are broken", () => {
    const start = source.indexOf("{#if mcpInfo.plugin_found}");
    expect(start).toBeGreaterThan(-1);
    const end = source.indexOf("{/if}", start);
    const block = source.slice(start, end);
    const found = block.slice(0, block.indexOf("{:else}"));
    expect(found).toContain("plugin_error");
  });

  // ADVERSARIAL (L1): `{#if mcpInfo.plugin_error}` is true for "   ", so a
  // whitespace-only error draws an empty amber span: the blank, unexplained row
  // this change set out to remove. The found branch must test the trimmed text.
  it("a whitespace-only plugin_error is treated as no error", () => {
    const start = source.indexOf("{#if mcpInfo.plugin_found}");
    const found = source.slice(start, source.indexOf("{:else}", start));
    expect(found).toMatch(/\{#if mcpInfo\.plugin_error\.trim\(\)\}/);
  });

  // ADVERSARIAL (L1): the error carries a filesystem path and an OS or JSON
  // message with no spaces to wrap at. Its siblings (binary path, hook path) use
  // `break-all`; the plugin row has no wrapping class, so a long error widens
  // the settings panel instead of wrapping.
  it("a long plugin_error wraps inside the row", () => {
    const start = source.indexOf("{#if mcpInfo.plugin_found}");
    const opener = source.slice(source.lastIndexOf("<div", start), start);
    const span = /<span[^>]*data-testid="plugin-error"[^>]*>/.exec(source)?.[0] ?? "";
    expect(`${opener} ${span}`).toMatch(/break-all|break-words|\[overflow-wrap:anywhere\]/);
  });
});
