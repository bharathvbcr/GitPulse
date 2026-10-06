/**
 * What the store does with the model half of an answer it did not write.
 *
 * Every case here is version skew. An older backend sends no `model_fields`
 * and would refuse a model setting it has never heard of, so the panel must
 * offer it none — not the built-in table, which describes this build's
 * backend and not that one. A newer backend can name a launcher or a field
 * this build cannot render; neither may reach a control.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...args: unknown[]) => invoke(...args) }));

const tauri = vi.fn(() => true);
vi.mock("../platform", () => ({ isTauri: () => tauri() }));

const { agentDefaults, loadAgentDefaults, saveAgentDefaults, resetAgentDefaultsForTests } = await import("./agentDefaultsStore");
const { EFFORT_LEVELS, MODEL_FIELDS_BY_LAUNCHER, PERMISSION_LAUNCHERS, PERMISSION_MODES } = await import("../terminal/agentDefaults");

const answer = (extra: Record<string, unknown>) => ({
  defaults: { permission: {} },
  modes: [...PERMISSION_MODES],
  launchers: [...PERMISSION_LAUNCHERS],
  ...extra,
});

beforeEach(() => {
  invoke.mockReset();
  tauri.mockReturnValue(true);
  resetAgentDefaultsForTests();
});

describe("model fields", () => {
  it("adopts the backend's map", async () => {
    invoke.mockResolvedValue(answer({ model_fields: { claude: ["model", "effort", "fallback", "advisor"], codex: ["model"] }, effort_levels: [...EFFORT_LEVELS] }));
    const view = await loadAgentDefaults();
    expect(view.modelFields).toEqual({ claude: ["model", "effort", "fallback", "advisor"], codex: ["model"] });
    expect(view.effortLevels).toEqual([...EFFORT_LEVELS]);
  });

  it("offers an older backend that sends no map no model controls at all", async () => {
    invoke.mockResolvedValue(answer({ defaults: { permission: {}, models: { claude: { model: "opus" } } } }));
    const view = await loadAgentDefaults();
    expect(view.modelFields).toEqual({});
    // And a stored model that backend could not have written is not adopted.
    expect(view.defaults.models).toBeUndefined();
  });

  it("drops launchers and fields this build cannot render, keeping its own order", async () => {
    invoke.mockResolvedValue(
      answer({
        model_fields: { claude: ["advisor", "teleport", "model"], hal9000: ["model"], shell: "model", grok: [] },
        effort_levels: ["high", "ludicrous", 7],
      }),
    );
    const view = await loadAgentDefaults();
    expect(view.modelFields).toEqual({ claude: ["model", "advisor"] });
    expect(view.effortLevels).toEqual(["high"]);
  });

  it("narrows stored models by the map the backend sent, not the built-in one", async () => {
    invoke.mockResolvedValue(
      answer({
        defaults: { permission: {}, models: { claude: { model: "opus", effort: "high" }, agy: { model: "gemini-3.8-flash-high" } } },
        model_fields: { claude: ["model"] },
      }),
    );
    const view = await loadAgentDefaults();
    expect(view.defaults.models).toEqual({ claude: { model: "opus" } });
  });

  it("survives a map of the wrong shape", async () => {
    for (const model_fields of [null, 5, "claude", [["claude", ["model"]]]]) {
      resetAgentDefaultsForTests();
      invoke.mockResolvedValue(answer({ model_fields }));
      const view = await loadAgentDefaults();
      expect(view.modelFields, JSON.stringify(model_fields)).toEqual({});
      expect(view.effortLevels.length).toBeGreaterThan(0);
    }
  });

  it("uses the built-in table only where there is no backend at all", async () => {
    tauri.mockReturnValue(false);
    const view = await loadAgentDefaults();
    expect(view.modelFields).toEqual(MODEL_FIELDS_BY_LAUNCHER);
    expect(invoke).not.toHaveBeenCalled();
  });

  it("adopts which launchers can list models, only where the launcher takes a model", async () => {
    invoke.mockResolvedValue(
      answer({
        model_fields: { claude: ["model"], agy: ["model", "effort"], codex: ["model"] },
        model_listing: { claude: "known", agy: "listed", codex: "teleport", grok: "listed", hal9000: "listed" },
      }),
    );
    const view = await loadAgentDefaults();
    expect(view.modelListing).toEqual({ claude: "known", agy: "listed" });
  });

  it("offers no model list to an older backend that cannot answer for one", async () => {
    invoke.mockResolvedValue(answer({ model_fields: { agy: ["model"] } }));
    expect((await loadAgentDefaults()).modelListing).toEqual({});
  });

  it("publishes the saved models the backend read back, not the draft", async () => {
    invoke.mockResolvedValue(
      answer({ defaults: { permission: {}, models: { claude: { model: "sonnet" } } }, model_fields: { claude: ["model", "effort"] } }),
    );
    const view = await saveAgentDefaults({ permission: {}, models: { claude: { model: "opus" } } });
    expect(view.defaults.models).toEqual({ claude: { model: "sonnet" } });
    expect(agentDefaults().defaults.models).toEqual({ claude: { model: "sonnet" } });
    expect(invoke).toHaveBeenCalledWith("cmd_agent_defaults_save", { defaults: { permission: {}, models: { claude: { model: "opus" } } } });
  });
});
