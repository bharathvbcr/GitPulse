/**
 * The model catalog as the settings row sees it.
 *
 * Every case is a way the answer could mislead a reader: a failed listing
 * shown as "no models", a malformed entry offered as a suggestion a save
 * would refuse, two buttons presses running the CLI twice, or the last good
 * list vanishing because a refresh failed.
 */
import { readFileSync } from "node:fs";
import { get } from "svelte/store";
import { beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...args: unknown[]) => invoke(...args) }));
vi.mock("../platform", () => ({ isTauri: () => true }));

const { MAX_CATALOG_MODELS, adoptCatalog, agentModelCatalogs, listedModel, loadAgentModels, resetAgentModelCatalogsForTests } = await import("./agentModelCatalog");

const rust = readFileSync(new URL("../../../src-tauri/src/workbench/agent_models.rs", import.meta.url), "utf8");

const listing = (models: unknown[], extra: Record<string, unknown> = {}) => ({
  launcher: "agy",
  models,
  listing: "listed",
  command: "agy models",
  fetched_at: 1_791_259_000_000,
  cached: false,
  skipped: 0,
  truncated: false,
  error: null,
  ...extra,
});

beforeEach(() => {
  invoke.mockReset();
  resetAgentModelCatalogsForTests();
});

describe("adoptCatalog", () => {
  it("keeps a well-formed answer exactly", () => {
    const raw = listing([{ id: "gemini-3.8-flash-high", label: "Gemini 3.8 Flash (High)", source: "cli" }]);
    expect(adoptCatalog(raw, "agy")).toEqual(raw);
  });

  it("drops and counts an entry a save would refuse, so no suggestion can be refused", () => {
    const out = adoptCatalog(
      listing([{ id: "ok-1", label: null, source: "cli" }, { id: "-x" }, { id: "a b" }, null, 7, { id: "ok-1", label: "dup", source: "cli" }], { skipped: 2 }),
      "agy",
    );
    expect(out?.models.map((m) => m.id)).toEqual(["ok-1"]);
    expect(out?.skipped).toBe(6);
  });

  it("never shows a failed or empty listing as an empty list", () => {
    expect(adoptCatalog(listing([], { error: "agy models exited with status 1: not signed in" }), "agy")?.error).toContain("not signed in");
    // Exit 0 with nothing usable is still not "this account has no models".
    expect(adoptCatalog(listing([]), "agy")?.error).toBeTruthy();
    expect(adoptCatalog(listing([{ id: "--x" }]), "agy")?.error).toBeTruthy();
  });

  it("refuses an answer of the wrong shape or for another launcher", () => {
    for (const raw of [null, undefined, "agy", [], { launcher: "agy" }, { launcher: "agy", models: "x" }, listing([], { launcher: "claude" })]) {
      expect(adoptCatalog(raw, "agy"), JSON.stringify(raw)).toBeNull();
    }
  });

  it("bounds the list at the backend's own cap", () => {
    expect(rust).toContain(`pub(crate) const MAX_CATALOG_MODELS: usize = ${MAX_CATALOG_MODELS};`);
    const flood = Array.from({ length: MAX_CATALOG_MODELS + 10 }, (_, i) => ({ id: `m-${i}`, label: null, source: "cli" }));
    const out = adoptCatalog(listing(flood), "agy");
    expect(out?.models).toHaveLength(MAX_CATALOG_MODELS);
    expect(out?.truncated).toBe(true);
  });
});

describe("loadAgentModels", () => {
  it("shares one request among concurrent asks", async () => {
    invoke.mockResolvedValue(listing([{ id: "m", label: null, source: "cli" }]));
    await Promise.all([loadAgentModels("agy"), loadAgentModels("agy", true), loadAgentModels("agy")]);
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke).toHaveBeenCalledWith("cmd_agent_models", { launcher: "agy", refresh: false });
  });

  it("passes a refresh through", async () => {
    invoke.mockResolvedValue(listing([{ id: "m", label: null, source: "cli" }]));
    await loadAgentModels("agy", true);
    expect(invoke).toHaveBeenCalledWith("cmd_agent_models", { launcher: "agy", refresh: true });
  });

  it("keeps the last good list visible while loading and after a failed refresh", async () => {
    invoke.mockResolvedValueOnce(listing([{ id: "kept", label: null, source: "cli" }]));
    await loadAgentModels("agy");
    let release: (value: unknown) => void = () => {};
    invoke.mockReturnValueOnce(new Promise((resolve) => (release = resolve)).then(() => Promise.reject(new Error("timed out"))));
    const refresh = loadAgentModels("agy", true);
    const loading = get(agentModelCatalogs).agy;
    expect(loading?.status).toBe("loading");
    expect(loading?.status === "loading" ? loading.previous?.models[0].id : null).toBe("kept");
    release(null);
    const failed = await refresh;
    expect(failed.status).toBe("failed");
    expect(failed.status === "failed" ? [failed.message, failed.previous?.models[0].id] : null).toEqual(["timed out", "kept"]);
  });

  it("keeps the last good list when a refresh answers with an error instead of throwing", async () => {
    invoke.mockResolvedValueOnce(listing([{ id: "kept", label: null, source: "cli" }]));
    await loadAgentModels("agy");
    invoke.mockResolvedValueOnce(listing([], { error: "agy models exited with status 1: not signed in" }));
    const state = await loadAgentModels("agy", true);
    expect(state).toEqual({ status: "failed", message: "agy models exited with status 1: not signed in", previous: expect.objectContaining({ models: [{ id: "kept", label: null, source: "cli" }] }) });
    // And a first ask that fails has nothing to keep.
    resetAgentModelCatalogsForTests();
    invoke.mockResolvedValueOnce(listing([], { error: "boom" }));
    expect(await loadAgentModels("agy")).toEqual({ status: "failed", message: "boom", previous: null });
  });

  it("reports a refused command as a failure, not as no models", async () => {
    invoke.mockRejectedValue("claude has no model list GitPulse can read.");
    const state = await loadAgentModels("codex");
    expect(state).toEqual({ status: "failed", message: "claude has no model list GitPulse can read.", previous: null });
  });
});

describe("listedModel", () => {
  it("answers only from a successful listing", async () => {
    invoke.mockResolvedValue(listing([{ id: "gemini-3.8-flash-high", label: null, source: "cli" }]));
    const state = await loadAgentModels("agy");
    expect(listedModel(state, "gemini-3.8-flash-high")).toBe(true);
    expect(listedModel(state, "gemini-9-ultra")).toBe(false);
    expect(listedModel(state, undefined)).toBeNull();
    expect(listedModel(undefined, "x")).toBeNull();
    // A failed listing knows nothing: no warning may be raised from it.
    expect(listedModel({ status: "ready", catalog: { ...listing([]), error: "boom" } as never }, "x")).toBeNull();
    expect(listedModel({ status: "failed", message: "x", previous: null }, "x")).toBeNull();
  });
});
