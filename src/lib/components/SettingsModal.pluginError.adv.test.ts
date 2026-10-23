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
});
