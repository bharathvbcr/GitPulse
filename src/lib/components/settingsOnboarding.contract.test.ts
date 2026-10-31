import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

/**
 * Source pins for Settings / setup-wizard wiring that lives in Svelte
 * templates. vitest runs in node with no component mount, so the behaviours
 * below cannot be driven here; what can be pinned is the one line each fix
 * turns on, so a revert fails loudly instead of waiting for a browser pass.
 */
const read = (name: string) => readFileSync(new URL(`./${name}`, import.meta.url), "utf8");

describe("Settings panels load once (SO-1)", () => {
  for (const name of ["LappiSettings.svelte", "HostLoadSetting.svelte", "SessionAlertSettings.svelte"]) {
    const source = read(name);
    it(`${name} gates its load effect on one recorded attempt, not on success`, () => {
      // `!loaded && !busy` re-fires as `busy` settles after every rejection.
      expect(source).not.toMatch(/active && !loaded/);
      expect(source).toContain("loader.request()");
    });
  }

  it("the Lappi and host-load panels offer Retry beside their error", () => {
    for (const name of ["LappiSettings.svelte", "HostLoadSetting.svelte"]) {
      expect(read(name)).toContain("loader.retry()");
    }
  });

  it("the notification panel shows the store's load failure instead of the default form", () => {
    const source = read("SessionAlertSettings.svelte");
    expect(source).toContain("sessionAlertsLoadError");
    expect(source).toContain("session-alerts-load-error");
  });
});

describe("Settings and Shortcuts Escape handling (SO-8)", () => {
  const settings = read("SettingsModal.svelte");
  const shortcuts = read("ShortcutsModal.svelte");

  it("both dialogs ignore an IME Escape", () => {
    expect(settings).toContain("isDialogEscape(e) && onClose?.()");
    expect(shortcuts).toContain("isDialogEscape(e) && close()");
    expect(settings).not.toContain('e.key === "Escape" && onClose');
    expect(shortcuts).not.toContain('e.key === "Escape" && close');
  });

  it("Escape in the Settings search clears it before it can close the dialog", () => {
    expect(settings).toContain("escapeClearsSearch(e, query)");
  });

  it("the '?' shortcut stands down while another modal is open", () => {
    expect(shortcuts).toContain("!isOpen && !isModalOccupied()");
  });
});

describe("Setup wizard surfaces failures (SO-2)", () => {
  const wizard = read("onboarding/SetupWizard.svelte");

  it("renders a failed read with Retry in both the Check and Install steps", () => {
    expect(wizard).toContain("retryWizardLoad($wizard.tool)");
    expect(wizard.match(/@render readFailure\(\)/g)).toHaveLength(2);
  });

  it("disables Install while any tool is installing, and Verify while it runs", () => {
    expect(wizard).toContain("disabled={$installing !== null ||");
    expect(wizard).toContain("disabled={$verifying}");
  });

  it("closes through finishSetup, which keeps the wizard open on failure", () => {
    expect(wizard).toContain("void finishSetup()");
    expect(wizard).not.toContain("markOnboardingComplete");
    expect(wizard).toContain("$actionError");
  });

  it("only says 'complete' for a passing verify", () => {
    expect(wizard).toContain('outcome === "passed"');
    expect(wizard).toContain("verify did not pass");
  });
});

describe("No native window.confirm in Settings panels (SO-12)", () => {
  // window.confirm answers null under WKWebView, so `!window.confirm(...)`
  // read as a refusal and the button silently did nothing.
  for (const name of ["ExternalToolsPanel.svelte", "AgentIntegrationPanel.svelte"]) {
    it(`${name} asks through the in-app dialog`, () => {
      expect(read(name)).not.toContain("window.confirm");
    });
  }

  it("uninstall goes through confirmAndUninstall and apply through askConfirm", () => {
    expect(read("ExternalToolsPanel.svelte")).toContain("confirmAndUninstall(tool)");
    expect(read("AgentIntegrationPanel.svelte")).toContain("await askConfirm(");
  });

  it("the Agents plugin copy names the integrations, uses <code> not backticks, and has a pending state", () => {
    const settings = read("SettingsModal.svelte");
    expect(settings).not.toContain("native Codex plugin package");
    expect(settings).toMatch(/Claude Code, Codex and Cursor/);
    expect(settings).not.toMatch(/`gitpulse-(mcp|hook)`/);
    expect(settings).toContain("Reading plugin info…");
  });

  it("Reset Tips says it worked and forgets when Settings closes", () => {
    const settings = read("SettingsModal.svelte");
    expect(settings).toContain("onclick={resetTips}");
    expect(settings).toContain('tipsStatus = "Tips restored"');
    expect(settings).toMatch(/if \(!isOpen\) tipsStatus = ""/);
  });
});

describe("Launch at login row (SO-5)", () => {
  const settings = read("SettingsModal.svelte");

  it("is seeded as loading, not as an unreadable plugin", () => {
    expect(settings).toContain('$state<LaunchAtLogin>({ state: "loading" })');
    expect(settings).not.toContain("still reading");
  });

  it("renders from describeLaunchAtLogin and does not re-seed on open", () => {
    expect(settings).toContain("describeLaunchAtLogin(launchAtLogin)");
    expect(settings).toContain("disabled={launchRow.disabled}");
    expect(settings.match(/state: "loading"/g)).toHaveLength(1);
  });
});

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
