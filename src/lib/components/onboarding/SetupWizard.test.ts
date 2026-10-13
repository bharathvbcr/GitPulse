import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const source = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "SetupWizard.svelte"), "utf8");

describe("SetupWizard DevCouncil install", () => {
  it("offers DevMap-only, analysis, and full presets", () => {
    expect(source).toContain("DEVCOUNCIL_PRESETS");
    expect(source).toContain("Copy command");
    expect(source).toContain("Run in terminal");
    expect(source).toContain("!installSpec.runnable");
    expect(source).not.toContain("uv run");
    expect(source).not.toContain("uv tool");
  });

  it("runs the documented command through Console, not the Manvi action gate", () => {
    expect(source).toContain("enqueueConsoleLaunch");
    expect(source).toContain("repoStore.setTerminalOpen(true)");
    expect(source).not.toContain("cmd_manvi_run_action");
  });
});

describe("SetupWizard dialog accessibility", () => {
  it("traps focus, closes on Escape, and restores focus through the shared action", () => {
    expect(source).toContain('import { trapFocus } from "../../ui/focusTrap"');
    expect(source).toMatch(/aria-modal="true"[\s\S]*?use:trapFocus/);
    expect(source).toContain('event.key !== "Escape"');
    expect(source).toContain("isImeComposition(event)");
    expect(source).toContain("onkeydown={onDialogKeydown}");
  });

  it("keeps the dialog role on the card, not the scrim", () => {
    expect(source).toMatch(/<div\s+class="gp-scrim[^>]*role="presentation"/);
    expect(source.match(/role="dialog"/g)).toHaveLength(1);
  });

  it("cannot Install before the selected tool's ladder has loaded", () => {
    expect(source).toContain("disabled={!$ladder ||");
  });
});
