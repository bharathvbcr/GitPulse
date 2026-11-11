import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, describe, expect, it, vi } from "vitest";
import { render } from "svelte/server";
import { get, writable } from "svelte/store";
import HarnessBadge from "./HarnessBadge.svelte";
import { MANVI_FOCUS_IDS } from "../ui/manviFocus";
import {
  harnessStore,
  type AiStatus,
  type HarnessState,
  type HarnessStatus,
  type PolicyVerdict,
} from "../stores/harnessStore";
import { interfaceStore } from "../stores/interfaceStore";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "HarnessBadge.svelte"),
  "utf8",
);

/** Every destination the header chips route to, in markup order. */
const chipTargets = [...source.matchAll(/openManvi\("([a-z]+)"\)/g)].map(
  (match) => match[1],
);

afterEach(() => {
  vi.restoreAllMocks();
  interfaceStore.reset();
});

/** Renders with the Manvi menu open, since the model picker lives inside it. */
function renderState(overrides: Partial<HarnessState>, open = true) {
  const state = writable({ ...get(harnessStore), ...overrides });
  vi.spyOn(harnessStore, "subscribe").mockImplementation(state.subscribe);
  return render(HarnessBadge, { props: { open } }).body;
}

function harnessStatus(overrides: Partial<HarnessStatus>): HarnessStatus {
  return { ...ready.harness!, ...overrides };
}

function verdictOf(status: PolicyVerdict["status"]): PolicyVerdict {
  return {
    status, checked: status !== "unchecked", target: "git push", rule: "no-force-push", severity: "hard",
    reason: "", demoted: "", grant_id: "", granted_by: "", widened: "", degraded: [], task_id: "", detail: "", detail_code: "",
  };
}

/** The chip button's opening tag. */
function chipTag(body: string): string {
  return body.match(/<button\b[^>]*data-manvi-chip[^>]*>/)?.[0] ?? "";
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
    // Markup order: the verdict snippet is declared ahead of the Manvi menu,
    // whose "Open Manvi" item owns the harness destination.
    expect(chipTargets).toEqual(["activity", "harness"]);
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
    expect(source).toContain("Open a repository to reach the Manvi view.");
    // Hover styling is gated on the same condition, so a dead chip does not
    // light up under the pointer.
    expect(source).not.toMatch(/[^:]hover:/);
  });
});

describe("HarnessBadge rendering", () => {
  it("offers model selection in the Manvi menu without an open repository", () => {
    const body = renderState({ ai: null, preferred: null, error: null, isProbing: false });
    const picker = body.match(/<select\b[^>]*>[\s\S]*?<\/select>/)?.[0];
    expect(picker).toBeDefined();
    expect(picker).toContain('aria-label="Local model"');
    // Idle, before a probe: the closed face is the short "not checked" status,
    // not a sentence. The empty-value option is still the Automatic choice.
    expect(picker).toContain("Not checked");
    expect(picker).toContain('value=""');
    expect(picker).not.toMatch(/<select[^>]*\sdisabled/);
  });

  it("renders the Manvi chip and its menu with distinct tooltips", () => {
    const body = renderState({}, true);
    expect(body).toContain("Manvi");
    const titles = [...body.matchAll(/title="([^"]*)"/g)].map(
      (match) => match[1],
    );
    expect(titles.length).toBeGreaterThanOrEqual(3);
    expect(new Set(titles).size).toBe(titles.length);
    // The chip carries the harness and model state; the menu's select keeps
    // the model tooltip; only the navigation item needs an open repository.
    expect(titles[0]).toContain("MANVI policy status has not been checked yet.");
    expect(titles[1]).toContain("Choose a local model.");
    expect(titles[1]).not.toContain("Open a repository");
    expect(titles.some((title) => title.includes("Open a repository to reach the Manvi view."))).toBe(true);
    expect(body).toContain("disabled");
  });
});

describe("HarnessBadge chip", () => {
  it("is one chip whose tone is the permission mode", () => {
    const cases: Array<[Partial<HarnessStatus> | null, string]> = [
      [{ available: true }, "ok"],
      [{ available: false, error_code: "not_installed" }, "warn"],
      [{ available: false, error_code: "timeout" }, "danger"],
      [null, "unknown"],
    ];
    for (const [harness, tone] of cases) {
      vi.restoreAllMocks();
      const body = renderState({ harness: harness ? harnessStatus(harness) : null }, false);
      const tag = chipTag(body);
      expect(tag, tone).toContain('class="gp-chip');
      expect(tag, tone).toContain(`data-tone="${tone}"`);
    }
  });

  it("opens a menu: haspopup, expanded, and the shield-only name keeps Manvi and the state", () => {
    const closed = chipTag(renderState({ harness: harnessStatus({ available: true }) }, false));
    expect(closed).toContain('aria-haspopup="menu"');
    expect(closed).toContain('aria-expanded="false"');
    expect(closed).toContain('aria-label="Manvi, policy checks connected"');
    vi.restoreAllMocks();
    const open = chipTag(renderState({}, true));
    expect(open).toContain('aria-expanded="true"');
    expect(source).toContain("restoreFocusTo(chipEl)");
    expect(source).toContain('escape: "capture"');
    expect(source).toContain("use:popover={menuPopover}");
    expect(source).toContain("use:portal");
    expect(source).toContain("LAYERS.MENU");
  });

  it("shows the label from 1100px and the model name from 1440px", () => {
    const body = renderState({ ai: ready, preferred: null }, false);
    expect(body).toContain('class="font-medium max-[1099px]:sr-only"');
    expect(body).toMatch(/class="hidden[^"]*min-\[1440px\]:inline"[^>]*>Automatic · chat</);
  });

  it("keeps the live model status rendered while the menu is closed", () => {
    const body = renderState({ ai: ready }, false);
    expect(body).toMatch(/id="[^"]+-model-status" class="sr-only" role="status"/);
    expect(body).toContain("Automatically using chat.");
    // The picker itself lives in the menu, so it is not mounted while closed.
    expect(body).not.toContain('aria-label="Local model"');
    expect(body).not.toContain("data-manvi-menu");
  });

  it("holds the picker, the refresh button and Open Manvi in the menu", () => {
    const body = renderState({ ai: ready }, true);
    const menu = body.slice(body.indexOf("data-manvi-menu"));
    expect(menu).toContain('role="menu"');
    expect(menu).toContain('aria-label="Local model"');
    expect(menu).toContain("data-manvi-face=");
    expect(menu).toContain('aria-label="Refresh local models"');
    expect(menu).toMatch(/role="menuitem"[^>]*>[\s\S]*Open Manvi/);
  });

  it("keeps a blocked or unchecked verdict inline, never behind the menu", () => {
    for (const status of ["blocked", "unchecked"] as const) {
      vi.restoreAllMocks();
      // A connected harness (ok tone) so the danger/unknown tone can only come from the verdict.
      const body = renderState({ harness: harnessStatus({ available: true }), lastVerdict: verdictOf(status) }, false);
      expect(body, status).toContain("gp-chip shrink-0 gap-1.5");
      expect(body, status).toContain(status === "blocked" ? 'data-tone="danger"' : 'data-tone="unknown"');
    }
  });

  it("puts a normal verdict in the menu as a menu item", () => {
    const closed = renderState({ lastVerdict: verdictOf("allowed") }, false);
    expect(closed).not.toContain("Policy: clean");
    vi.restoreAllMocks();
    const open = renderState({ lastVerdict: verdictOf("allowed") }, true);
    const menu = open.slice(open.indexOf("data-manvi-menu"));
    expect(menu).toContain("Policy: clean");
    expect(menu).toMatch(/role="menuitem"[^>]*class="gp-menu-item[^"]*"[^>]*>[\s\S]*Policy: clean/);
  });

  it("marks a failed model check on the chip itself", () => {
    const body = renderState({ ai: ready, error: "Request timed out" }, false);
    expect(chipTag(body)).toBeTruthy();
    expect(body).toContain("text-danger");
    expect(body).toContain("Failed");
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
    expect(body).toContain('aria-label="Try local model discovery again"');
    expect(body).toContain("Couldn't check local models. Request timed out");
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
    expect(body).toMatch(/<option value="" selected="">No models<\/option>/);
    expect(body).toMatch(/data-manvi-face="No models"/);
    expect(body).toContain("Start a local model server, then refresh.");
    expect(body).toContain("No local models available");
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

  it("keeps the title-bar face short and the failure on the tooltip", () => {
    const error = "Cannot read properties of undefined (reading 'invoke')";
    const body = renderState({ error, isProbing: false });
    expect(body).toMatch(/data-manvi-face="Failed"/);
    expect(body).not.toMatch(
      /data-manvi-face="[^"]*(Retry|Cannot read properties|No local models available|retry needed|Automatic · retry)/,
    );
    const option = body.match(/<option value=""[^>]*>[^<]*<\/option>/)?.[0] ?? "";
    expect(option).toContain(">Failed<");
    expect(option).not.toContain("Retry");
    expect(option).not.toContain("Cannot read properties");
    expect(option).not.toContain("No local models available");
    expect(body).toContain('aria-label="Try local model discovery again"');
    expect(body).not.toMatch(/<option[^>]*>[^<]*No local models available/);
    expect(body).not.toMatch(/<option[^>]*>[^<]*Looking for local models/);
    // The longer reason is still in the tooltip and the live status.
    expect(body).toContain(`Couldn't check local models. ${error} Refresh to try again.`);
    expect(body).toContain("No local models available");
    expect(body).toContain("Choose a local model.");
  });

  it("shortens the other closed faces without inventing a status", () => {
    expect(renderState({ isProbing: true })).toMatch(/data-manvi-face="Checking…"/);
    vi.restoreAllMocks();
    expect(renderState({ ai: null, error: null, isProbing: false, preferred: null })).toMatch(
      /data-manvi-face="Not checked"/,
    );
    vi.restoreAllMocks();
    const model = "very-long-local-model-name-that-would-crowd-the-title-bar";
    const crowded = renderState({
      ai: { ...ready, selected: { base_url: ready.endpoints[0].base_url, model } },
      preferred: null,
    });
    expect(crowded).toMatch(new RegExp(`data-manvi-face="Automatic · ${model}"`));
    // The 160px cap moved with the picker into the menu, which is 288px wide:
    // the face there fills its row and ellipsizes; the chip's face is capped at 10rem.
    expect(crowded).toContain("max-w-40");
    expect(crowded).toContain("truncate");
  });

  it("renders no title-bar badge when the setting is off", () => {
    const app = readFileSync(
      join(dirname(fileURLToPath(import.meta.url)), "../../App.svelte"),
      "utf8",
    );
    expect(app).toMatch(/\{#if \$interfaceStore\.showHarnessBadges\}\s*<HarnessBadge \/>/);
    const ungated = app.replace(
      /\{#if \$interfaceStore\.showHarnessBadges\}[\s\S]*?\{\/if\}/,
      "",
    );
    expect(ungated).not.toMatch(/<HarnessBadge(?:\s|\/>)/);

    interfaceStore.setShowHarnessBadges(false);
    const body = render(HarnessBadge).body;
    expect(body).not.toContain("MANVI");
    expect(body).not.toContain("data-manvi-face");
    expect(body).not.toContain('aria-label="Local model"');
    expect(body).not.toContain('data-testid="manvi-titlebar-status"');
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
