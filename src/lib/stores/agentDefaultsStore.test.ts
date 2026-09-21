/**
 * What the store does with an answer it did not write.
 *
 * `adopt` is the boundary between "what the backend said" and "what this build
 * can render". The cases that matter are all version skew: an older or newer
 * binary can legitimately name a mode or a launcher this app has no label for,
 * and such a value must never reach a chooser.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...args: unknown[]) => invoke(...args) }));

const tauri = vi.fn(() => true);
vi.mock("../platform", () => ({ isTauri: () => tauri() }));

const {
  agentDefaults,
  loadAgentDefaults,
  saveAgentDefaults,
  resetAgentDefaultsForTests,
  DEFAULT_AGENT_DEFAULTS_VIEW,
} = await import("./agentDefaultsStore");
const { PERMISSION_MODES, PERMISSION_LAUNCHERS, BYPASS_MODE } = await import(
  "../terminal/agentDefaults"
);

beforeEach(() => {
  invoke.mockReset();
  tauri.mockReturnValue(true);
  resetAgentDefaultsForTests();
});

describe("loadAgentDefaults", () => {
  it("adopts a well-formed answer", async () => {
    invoke.mockResolvedValue({
      defaults: { permission: { claude: "edit" } },
      modes: [...PERMISSION_MODES],
      launchers: [...PERMISSION_LAUNCHERS],
    });
    const view = await loadAgentDefaults();
    expect(view.defaults.permission.claude).toBe("edit");
    expect(view.modes).toEqual([...PERMISSION_MODES]);
  });

  it("loads once and caches, because every tab reads it before spawning", async () => {
    invoke.mockResolvedValue({ defaults: { permission: {} }, modes: [], launchers: [] });
    await loadAgentDefaults();
    await loadAgentDefaults();
    await loadAgentDefaults();
    expect(invoke).toHaveBeenCalledTimes(1);
  });

  it("coalesces concurrent first reads into one round trip", async () => {
    invoke.mockResolvedValue({ defaults: { permission: {} }, modes: [], launchers: [] });
    await Promise.all([loadAgentDefaults(), loadAgentDefaults(), loadAgentDefaults()]);
    expect(invoke).toHaveBeenCalledTimes(1);
  });

  /**
   * A settings file that cannot be read must not be the reason a terminal
   * will not open. The fallback is "every CLI on its own", which is the
   * shipped behaviour and the least authority this feature can grant.
   */
  it("falls back to the shipped defaults when the backend fails", async () => {
    invoke.mockRejectedValue(new Error("no config"));
    const view = await loadAgentDefaults();
    expect(view.defaults).toEqual({ permission: {} });
    expect(agentDefaults().defaults).toEqual({ permission: {} });
  });

  it("returns the shipped defaults outside Tauri without any IPC", async () => {
    tauri.mockReturnValue(false);
    const view = await loadAgentDefaults();
    expect(view).toEqual(DEFAULT_AGENT_DEFAULTS_VIEW);
    expect(invoke).not.toHaveBeenCalled();
  });

  it("drops modes and launchers this build cannot render", async () => {
    invoke.mockResolvedValue({
      defaults: { permission: { claude: "edit" } },
      modes: ["edit", "teleport", "", null, 7],
      launchers: ["claude", "hal9000", null],
    });
    const view = await loadAgentDefaults();
    expect(view.modes).toEqual(["edit"]);
    expect(view.launchers).toEqual(["claude"]);
  });

  /**
   * An answer naming no usable mode falls back to the full list rather than
   * to an empty chooser: an empty select reads as "this feature is broken",
   * while the full list is at worst a choice the backend then refuses loudly.
   */
  it("falls back to the full lists when nothing in the answer is usable", async () => {
    invoke.mockResolvedValue({ defaults: {}, modes: ["nope"], launchers: ["nope"] });
    const view = await loadAgentDefaults();
    expect(view.modes).toEqual([...PERMISSION_MODES]);
    expect(view.launchers).toEqual([...PERMISSION_LAUNCHERS]);
  });

  it("narrows stored defaults against the launchers the backend admits", async () => {
    // The backend says only claude has a policy; a stored codex mode is then
    // not something this session may apply.
    invoke.mockResolvedValue({
      defaults: { permission: { claude: "edit", codex: BYPASS_MODE } },
      modes: [...PERMISSION_MODES],
      launchers: ["claude"],
    });
    const view = await loadAgentDefaults();
    expect(view.defaults.permission).toEqual({ claude: "edit" });
  });

  it("survives a malformed answer of the wrong shape entirely", async () => {
    for (const answer of [null, undefined, 42, "nope", [], { defaults: 5 }]) {
      resetAgentDefaultsForTests();
      invoke.mockResolvedValue(answer);
      const view = await loadAgentDefaults();
      expect(view.defaults).toEqual({ permission: {} });
      expect(view.modes.length).toBeGreaterThan(0);
    }
  });
});

describe("agentDefaults", () => {
  it("is readable synchronously before any load, for the spawn path", () => {
    // The spawn cannot await. It must get an answer, and the answer before a
    // load is the shipped one.
    expect(agentDefaults()).toEqual(DEFAULT_AGENT_DEFAULTS_VIEW);
  });

  it("reflects the adopted answer after a load", async () => {
    invoke.mockResolvedValue({
      defaults: { permission: { grok: "inspect" } },
      modes: [...PERMISSION_MODES],
      launchers: [...PERMISSION_LAUNCHERS],
    });
    await loadAgentDefaults();
    expect(agentDefaults().defaults.permission.grok).toBe("inspect");
  });
});

describe("saveAgentDefaults", () => {
  it("publishes the backend's adopted answer, not the draft it sent", async () => {
    // The backend is the authority on what was stored. Publishing the draft
    // would show a setting that was never saved if the backend narrowed it.
    invoke.mockResolvedValue({
      defaults: { permission: { claude: "edit" } },
      modes: [...PERMISSION_MODES],
      launchers: [...PERMISSION_LAUNCHERS],
    });
    const view = await saveAgentDefaults({ permission: { claude: BYPASS_MODE } });
    expect(view.defaults.permission.claude).toBe("edit");
    expect(agentDefaults().defaults.permission.claude).toBe("edit");
  });

  it("propagates a refusal rather than reporting a save that did not happen", async () => {
    invoke.mockRejectedValue(new Error("Too many agent permission defaults"));
    await expect(saveAgentDefaults({ permission: {} })).rejects.toThrow(
      "Too many agent permission defaults",
    );
  });

  it("updates in-memory only outside Tauri", async () => {
    tauri.mockReturnValue(false);
    const view = await saveAgentDefaults({ permission: { claude: "ask" } });
    expect(view.defaults.permission.claude).toBe("ask");
    expect(invoke).not.toHaveBeenCalled();
  });
});
