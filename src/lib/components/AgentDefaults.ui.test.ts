/**
 * Contracts for the two surfaces agent launch defaults touch: the settings
 * pane that stores them and the terminal session that acts on one.
 *
 * Source-read, like the other component contracts here — these are structural
 * properties ("nothing spawns before the reader agrees", "the chooser is
 * derived, not hand-listed") that a rendering test would assert less directly
 * and a reviewer would have to re-derive by hand.
 */
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { SETTINGS_CATALOG } from "../ui/settingsCatalog";

const here = dirname(fileURLToPath(import.meta.url));
const panel = readFileSync(join(here, "AgentDefaultsSettings.svelte"), "utf8");
const liveRuns = readFileSync(join(here, "AgentLiveRunsSetting.svelte"), "utf8");
const sources = readFileSync(join(here, "AgentSettingSourcesSetting.svelte"), "utf8");
const session = readFileSync(join(here, "TerminalSession.svelte"), "utf8");
const modal = readFileSync(join(here, "SettingsModal.svelte"), "utf8");

describe("agents running at once", () => {
  it("is findable by the words a reader would type, in its own wrapper", () => {
    const entry = SETTINGS_CATALOG.find((row) => row.id === "agent-live-runs");
    expect(entry?.section).toBe("agents");
    for (const word of ["concurrent", "parallel", "many", "sessions", "manvi"]) {
      expect(entry?.keywords, `"${word}" does not find this setting`).toContain(word);
    }
    // Its own wrapper in the modal: inside the permission panel's, a search
    // for "concurrent" hid it along with the permission rows.
    expect(modal).toMatch(/data-setting="agent-live-runs"[^>]*>\s*<AgentLiveRunsSetting/);
    expect(panel).not.toContain("max_live_runs");
  });

  it("takes its bounds from the store's constants, not a literal", () => {
    // The capacity contract ties these to dc-store; a 64 or an 8 written
    // here would be a third copy nothing checks.
    expect(liveRuns).toContain("max={MAX_LIVE_RUNS}");
    expect(liveRuns).toMatch(/\{MAX_LIVE_RUNS\}.*\{DEFAULT_LIVE_RUNS\}/s);
    expect(liveRuns.replace(/<!--[\s\S]*?-->|\/\*[\s\S]*?\*\//g, "")).not.toMatch(/\b(64|8)\b(?![\d.])/);
  });

  it("stores the default as absence and carries the permission defaults through", () => {
    expect(liveRuns).toContain("delete rest.max_live_runs");
    expect(liveRuns).toContain("next === DEFAULT_LIVE_RUNS ? rest");
    expect(liveRuns).toContain("const rest = { ...view.defaults };");
  });
});

describe("Claude settings files", () => {
  it("is findable by the words a reader would type, in its own wrapper", () => {
    const entry = SETTINGS_CATALOG.find((row) => row.id === "agent-setting-sources");
    expect(entry?.section).toBe("agents");
    for (const word of ["setting-sources", "project", "hooks", "settings.local.json"]) {
      expect(entry?.keywords, `"${word}" does not find this setting`).toContain(word);
    }
    expect(modal).toMatch(/data-setting="agent-setting-sources"[^>]*>\s*<AgentSettingSourcesSetting/);
    expect(panel).not.toContain("claude_setting_sources");
  });

  it("offers the sources the backend knows and stores every-source as absence", () => {
    // The list is the mirrored constant, never a literal here.
    expect(sources).toContain("{#each CLAUDE_SETTING_SOURCES as source (source)}");
    expect(sources).toContain("delete rest.claude_setting_sources");
    expect(sources).toContain("canonical === undefined ? rest");
    expect(sources).toContain("const rest = { ...view.defaults };");
    // Unchecking the last source is refused here, not sent to be refused.
    expect(sources).toMatch(/canonical === null\) \{[\s\S]*?input\.checked = true;[\s\S]*?return;/);
  });
});

describe("the settings pane", () => {
  it("lives in the agents section and is reachable from the catalog", () => {
    const entry = SETTINGS_CATALOG.find((row) => row.id === "agent-defaults");
    expect(entry, "no catalog entry for agent-defaults").toBeTruthy();
    expect(entry?.section).toBe("agents");
    // Searchable by the words a reader would actually type, including the
    // dangerous one — a setting nobody can find is a setting nobody has.
    for (const word of ["permission", "skip permissions", "bypass", "sandbox"]) {
      expect(entry?.keywords, `"${word}" does not find this setting`).toContain(word);
    }
  });

  it("is rendered by the modal inside the agents branch", () => {
    expect(modal).toContain("<AgentDefaultsSettings");
    expect(modal).toContain('data-setting="agent-defaults"');
  });

  /**
   * The chooser must come from what the backend said it can expand. A
   * hand-written list here would be a second copy of the policy table's shape
   * and would offer modes that refuse at spawn once the two drifted.
   */
  it("derives its rows and options from the backend's lists, not a literal", () => {
    expect(panel).toContain("{#each view.launchers as launcher");
    expect(panel).toContain("{#each view.modes as mode");
    // No launcher or mode spelled into the markup.
    for (const literal of ["claude", "codex", "grok", "agy", "acceptEdits", "bypassPermissions"]) {
      expect(panel, `${literal} is written into the panel`).not.toContain(`"${literal}"`);
    }
  });

  it("offers 'the CLI's own default' as a real choice, not an empty select", () => {
    // Absence is a distinct answer from every mode, and it is the shipped one.
    expect(panel).toContain("const INHERIT = \"\";");
    expect(panel).toContain("delete next[launcher]");
    expect(panel).toContain("own default");
  });

  it("says which launchers currently start without permission checks", () => {
    expect(panel).toContain('data-testid="agent-defaults-bypass-note"');
    // Named, not counted: a reader who set this months ago needs to know
    // which agent it applies to without opening every select.
    expect(panel).toContain("bypassing.map(launcherLabel).join");
  });

  it("surfaces a refused save rather than showing a setting that did not store", () => {
    expect(panel).toContain('data-testid="agent-defaults-error"');
    expect(panel).toContain("error = formatError(err)");
  });
});

describe("the terminal session's bypass gate", () => {
  it("passes the resolved mode to the spawn", () => {
    expect(session).toContain("permissionMode,");
    expect(session).toContain("resolvePermissionMode()");
  });

  /**
   * The gate itself. Mount must not spawn when the resolved mode needs an
   * acknowledgement that has not been given.
   */
  it("does not spawn while an acknowledgement is outstanding", () => {
    const mount = session.slice(session.indexOf("onMount(() => {"));
    const guard = mount.indexOf("requiresAcknowledgement(permissionMode) && !acknowledgedBypass");
    expect(guard, "mount does not gate on the acknowledgement").toBeGreaterThan(-1);
    const spawn = mount.indexOf("void spawnPty();", guard);
    const early = mount.indexOf("return;", guard);
    expect(early, "the gate does not return before spawning").toBeGreaterThan(-1);
    expect(early).toBeLessThan(spawn);
  });

  it("sends an acknowledgement only for the mode that needs one", () => {
    // The backend refuses an acknowledgement attached to any other mode, so
    // an unconditional `true` here would fail on the next ordinary launch.
    expect(session).toContain(
      "acknowledged: requiresAcknowledgement(permissionMode) ? acknowledgedBypass : false,",
    );
  });

  it("offers a way out that still opens a terminal, by narrowing rather than refusing", () => {
    const decline = session.slice(session.indexOf("function declineBypass()"));
    expect(decline.slice(0, decline.indexOf("\n  }"))).toContain('permissionMode = "ask"');
  });

  /**
   * A task run's permission mode was chosen for that run in the handoff form.
   * A host-wide default must not re-decide it.
   */
  it("leaves a task run's own permission mode alone", () => {
    const resolve = session.slice(session.indexOf("function resolvePermissionMode()"));
    expect(resolve.slice(0, resolve.indexOf("\n  }"))).toContain("if (taskRunId) return null;");
  });

  it("reads the defaults before the spawn, since they become argv", () => {
    expect(session).toContain("loadAgentDefaults()");
    const mount = session.slice(session.indexOf("onMount(() => {"));
    expect(mount.indexOf("loadAgentDefaults()")).toBeLessThan(mount.indexOf("void spawnPty();"));
  });

  it("covers the grid rather than sitting beside it, so nothing reads as started", () => {
    expect(session).toContain('data-terminal-acknowledge');
    expect(session).toContain('role="alertdialog"');
  });
});
