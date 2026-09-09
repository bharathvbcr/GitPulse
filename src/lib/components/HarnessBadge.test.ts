import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, describe, expect, it, vi } from "vitest";
import { render } from "svelte/server";
import { get, writable } from "svelte/store";
import HarnessBadge from "./HarnessBadge.svelte";
import { MANVI_FOCUS_IDS } from "../ui/manviFocus";
import { harnessStore, type AiStatus, type HarnessState } from "../stores/harnessStore";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "HarnessBadge.svelte"),
  "utf8",
);

/** Every destination the header chips route to, in markup order. */
const chipTargets = [...source.matchAll(/openManvi\("([a-z]+)"\)/g)].map(
  (match) => match[1],
);

afterEach(() => vi.restoreAllMocks());

function renderState(overrides: Partial<HarnessState>) {
  const state = writable({ ...get(harnessStore), ...overrides });
  vi.spyOn(harnessStore, "subscribe").mockImplementation(state.subscribe);
  return render(HarnessBadge).body;
}

const ready: AiStatus = {
  harness: { available: true, binary: "manvi", protocol: 1, posture: "guarded", ops: [], error: "", error_code: "" },
  endpoints: [
    { base_url: "http://localhost:11434/v1", reachable: true, detail: "", models: ["chat", "coder"] },
    { base_url: "http://localhost:1234/v1", reachable: true, detail: "", models: ["chat"] },
    { base_url: "http://localhost:9999/v1", reachable: false, detail: "offline", models: ["stale-model"] },
  ],
  selected: { base_url: "http://localhost:11434/v1", model: "chat" },
  model_info: null, model_detail: "", ready: true, detail: "Ready",
};

describe("HarnessBadge destinations", () => {
  it("sends each chip to its own MANVI section", () => {
    // The gap: shield, model and verdict all called setActiveTab("manvi"),
    // which lands on the Ops pane — a different pane from two of the three
    // subjects, with the third several cards down the page.
    // The model picker now acts in place; only repository navigation chips
    // retain section destinations.
    expect(chipTargets).toEqual(["harness", "activity"]);
    expect(new Set(chipTargets).size).toBe(chipTargets.length);
    for (const target of chipTargets) {
      expect(MANVI_FOCUS_IDS).toContain(target);
    }
  });

  it("routes every chip through the focus request, never straight to the tab", () => {
    expect(source.match(/setActiveTab\("work", "policy"\)/g)).toHaveLength(1);
    expect(source).toContain("requestManviFocus(target);");
  });

  it("tells each chip's tooltip where the click lands", () => {
    // Guarded, so an empty match list cannot pass this as if it had checked.
    expect(chipTargets.length).toBe(2);
    for (const target of chipTargets) {
      expect(source).toContain(`manviFocusHint("${target}")`);
    }
  });
});

describe("HarnessBadge reachability", () => {
  it("does not offer a link to a view that cannot open", () => {
    // MANVI is a repository view: with no repository open there is no session
    // to switch tabs on, so the click did nothing at all. The chip keeps
    // reporting status; it just stops claiming to be a link.
    expect(source).toContain("let reachable = $derived(Boolean($repoStore.currentPath));");
    expect(source.match(/disabled=\{!reachable\}/g)).toHaveLength(
      chipTargets.length,
    );
    expect(source).toContain("Open a repository to reach the MANVI view.");
    // Hover styling is gated on the same condition, so a dead chip does not
    // light up under the pointer.
    expect(source).not.toMatch(/[^:]hover:/);
  });
});

describe("HarnessBadge rendering", () => {
  it("offers model selection directly in the header without an open repository", () => {
    const { body } = render(HarnessBadge);
    const picker = body.match(/<select\b[^>]*>[\s\S]*?<\/select>/)?.[0];
    expect(picker).toBeDefined();
    expect(picker).toContain('aria-label="Local model"');
    expect(picker).toContain("Automatic");
    expect(picker).not.toMatch(/<select[^>]*\sdisabled/);
  });

  it("renders the harness and model chips with distinct tooltips", () => {
    const { body } = render(HarnessBadge);
    expect(body).toContain("MANVI");
    const titles = [...body.matchAll(/title="([^"]*)"/g)].map(
      (match) => match[1],
    );
    expect(titles.length).toBeGreaterThanOrEqual(2);
    expect(new Set(titles).size).toBe(titles.length);
    // Only navigation needs an open repository; the model preference is global.
    expect(titles[0]).toContain("Open a repository to reach the MANVI view.");
    expect(titles[1]).toContain("Choose a local model.");
    expect(titles[1]).not.toContain("Open a repository");
    expect(body).toContain("disabled");
  });
});

describe("HarnessBadge model options", () => {
  it("provides an explicit refresh button without probing on keyboard focus", () => {
    const body = renderState({ ai: ready });
    expect(body).toContain('aria-label="Refresh local models"');
    expect(source).not.toContain("onfocus=");
  });

  it("announces the requested model while checking instead of the previous result", () => {
    const body = renderState({ ai: ready, preferred: { base_url: ready.endpoints[0].base_url, model: "coder" }, isProbing: true });
    expect(body).toContain('role="status"');
    expect(body).toContain("Checking coder…");
    expect(body).not.toContain("chat at http://localhost:11434/v1");
    expect(body).not.toMatch(/\sbg-accent\/10[\s"]/);
    expect(body).toMatch(/<button[^>]*aria-label="Refreshing local models"[^>]*disabled/);
  });

  it("offers retry after an error without presenting the stale result as ready", () => {
    const body = renderState({ ai: ready, error: "Request timed out" });
    expect(body).toContain('aria-label="Retry model discovery"');
    expect(body).toContain("Model check failed. Request timed out");
    expect(body).not.toContain("Automatically using chat");
    expect(body).not.toMatch(/\sbg-accent\/10[\s"]/);
  });

  it("describes the picker through an atomic live status message", () => {
    const body = renderState({ ai: ready });
    const description = body.match(/aria-describedby="([^"]+-model-status)"/)?.[1];
    expect(description).toBeDefined();
    expect(body).toContain(`id="${description}"`);
    expect(body).toContain('aria-live="polite" aria-atomic="true"');
    expect(body).toContain("Automatically using chat.");
  });

  it("does not report a different saved selection as ready", () => {
    const body = renderState({ ai: ready, preferred: { base_url: ready.endpoints[1].base_url, model: "chat" } });
    expect(body).not.toContain("Using chat.");
    expect(body).toContain("chat is unavailable. Choose another model or refresh.");
    expect(body).not.toMatch(/\sbg-accent\/10[\s"]/);
  });

  it("makes an empty discovery result visible in the selected Automatic option", () => {
    const body = renderState({ ai: { ...ready, endpoints: [], ready: false, selected: null } });
    expect(body).toMatch(/<option value="" selected="">Automatic · no models<\/option>/);
    expect(body).toContain("Start a local model server, then refresh.");
  });

  it("groups models by server and distinguishes identical model names", () => {
    const body = renderState({ ai: ready });
    expect(body).toContain('label="http://localhost:11434/v1"');
    expect(body).toContain('label="http://localhost:1234/v1"');
    expect(body).not.toContain("stale-model");
    const chats = [...body.matchAll(/<option value="([^"]*)"[^>]*>chat<\/option>/g)];
    expect(chats).toHaveLength(2);
    expect(chats[0][1]).not.toBe(chats[1][1]);
  });

  it("shows the automatic model without pinning it", () => {
    const body = renderState({ ai: ready, preferred: null });
    expect(body).toMatch(/<option value="" selected="">Automatic · chat<\/option>/);
  });

  it("selects the saved server and model even while its new probe is pending", () => {
    const body = renderState({ ai: ready, preferred: { base_url: ready.endpoints[1].base_url, model: "chat" }, isProbing: true });
    expect(body).toContain('aria-busy="true"');
    expect(body).toMatch(/<option value="[^"<>]*1234[^"<>]*" selected="">chat<\/option>/);
    expect(body).not.toMatch(/<option value="[^"<>]*11434[^"<>]*" selected/);
  });

  it("preserves a missing saved model while allowing Automatic recovery", () => {
    const body = renderState({ ai: { ...ready, endpoints: [], selected: null, ready: false }, preferred: { base_url: ready.endpoints[0].base_url, model: "missing" } });
    expect(body).toMatch(/<option[^>]*selected[^>]*>missing \(unavailable\)<\/option>/);
    expect(body).toContain("No local models available");
    expect(body).toContain('<option value="">Automatic</option>');
  });

  it("distinguishes discovery in progress from an empty or failed result", () => {
    expect(renderState({ isProbing: true })).toContain("Looking for local models…");
    vi.restoreAllMocks();
    const failed = renderState({ error: "Model request failed", isProbing: false });
    expect(failed).toContain("Model request failed");
    expect(failed).toContain("No local models available");
    expect(failed).not.toContain("Looking for local models…");
  });
});
