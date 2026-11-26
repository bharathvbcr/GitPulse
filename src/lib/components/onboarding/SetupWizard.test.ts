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

describe("SetupWizard with no repository open", () => {
  // The Console runs through cmd_terminal_run, which validates a repository
  // path, so there is no terminal to run in. The button is not rendered (it
  // used to be a disabled control with its reason in a tooltip), the reason is
  // said on the page, and Copy command keeps working.
  it("renders Run in terminal only when a repository is open, and says why otherwise", () => {
    expect(source).toMatch(/\{#if repoPath\}\s*<button[\s\S]*?Run in terminal[\s\S]*?\{\/if\}/);
    expect(source).not.toMatch(/disabled=\{!repoPath/);
    expect(source).toContain('data-testid="no-repo-terminal-note"');
    expect(source).toContain("{#if !repoPath}");
  });

  it("keeps Copy command independent of the repository", () => {
    const copy = source.slice(source.indexOf("Copy command") - 400, source.indexOf("Copy command"));
    expect(copy).not.toContain("repoPath");
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
    // Still gated on the ladder; SO-2 added "while any tool is installing" in front.
    expect(source).toMatch(/disabled=\{\$installing !== null \|\|\s+!\$ladder \|\|/);
  });

  it("displays Manvi's BrandMark in the wizard header and manvi tool selector", () => {
    expect(source).toContain('import BrandMark from "../BrandMark.svelte"');
    expect(source).toMatch(/\{#if \$wizard\.tool === "manvi"\}\s*<BrandMark mark="manvi"/);
    expect(source).toMatch(/\{#if t === "manvi"\}\s*<BrandMark mark="manvi"/);
  });
});
