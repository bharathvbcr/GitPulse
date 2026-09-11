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
    expect(source).toContain("setTerminalDockOpen(true)");
    expect(source).not.toContain("cmd_manvi_run_action");
  });
});
