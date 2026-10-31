import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

/**
 * Source pins for Settings / setup-wizard wiring that lives in Svelte
 * templates. vitest runs in node with no component mount, so the behaviours
 * below cannot be driven here; what can be pinned is the one line each fix
 * turns on, so a revert fails loudly instead of waiting for a browser pass.
 */
const read = (name: string) => readFileSync(new URL(`./${name}`, import.meta.url), "utf8");

describe("Run setup from Settings (SO-3)", () => {
  const settings = read("SettingsModal.svelte");
  const wizard = read("onboarding/SetupWizard.svelte");

  it("closes Settings before it asks for the wizard", () => {
    const body = /function runToolsSetup\(\) \{([\s\S]*?)\n  \}/.exec(settings)?.[1] ?? "";
    expect(body.indexOf("onClose?.()")).toBeGreaterThanOrEqual(0);
    expect(body.indexOf("onClose?.()")).toBeLessThan(body.indexOf("gitpulse:setup-tools"));
  });

  it("points the Run setup button at that function, not a bare dispatch", () => {
    expect(settings).toContain("onclick={runToolsSetup}");
    expect(settings).not.toMatch(/onclick=\{\(\) => window\.dispatchEvent\(new CustomEvent\("gitpulse:setup-tools"\)\)\}/);
  });

  it("stacks the wizard on the PROMPT layer, above the MODAL layer Settings uses", () => {
    expect(wizard).toContain("style=\"z-index: {LAYERS.PROMPT}\"");
    expect(wizard).not.toMatch(/\bz-50\b/);
  });
});
