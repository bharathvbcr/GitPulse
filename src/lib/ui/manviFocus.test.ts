import { existsSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { beforeEach, describe, expect, it } from "vitest";
import { get } from "svelte/store";
import {
  MANVI_FOCUS_IDS,
  MANVI_FOCUS_LIST,
  MANVI_FOCUS_TARGETS,
  manviFocusHint,
  manviFocusRequest,
  manviSectionId,
  requestManviFocus,
  takeManviFocus,
  takeManviFocusFor,
  type ManviFocusId,
} from "./manviFocus";

const componentDir = join(dirname(fileURLToPath(import.meta.url)), "..", "components");

function componentSource(name: string): string {
  return readFileSync(join(componentDir, name), "utf8");
}

/**
 * The component that renders each target's card. Policy's two panes are gone,
 * so each card has exactly one owner on the surface its subject belongs to.
 */
const CARD_OWNER: Record<ManviFocusId, string> = {
  harness: "ManviConnection.svelte",
  model: "LocalModelServers.svelte",
  activity: "PolicyActivity.svelte",
  cleanup: "BranchTagCleanup.svelte",
};

/**
 * The component that consumes requests for each destination kind: it opens
 * the right section or disclosure, then scrolls to the anchor.
 */
const CONSUMER = {
  settings: "SettingsModal.svelte",
  view: "WorkView.svelte",
} as const;

describe("Manvi focus catalog", () => {
  it("registers every target against a live destination, keyed by its own id", () => {
    for (const id of MANVI_FOCUS_IDS) {
      const target = MANVI_FOCUS_TARGETS[id];
      expect(target.id).toBe(id);
      expect(target.label.length).toBeGreaterThan(0);
      expect(["settings", "view"]).toContain(target.destination.kind);
    }
    expect(MANVI_FOCUS_LIST).toHaveLength(MANVI_FOCUS_IDS.length);
  });

  it("derives a distinct DOM anchor per target", () => {
    const anchors = MANVI_FOCUS_LIST.map((target) => manviSectionId(target.id));
    expect(new Set(anchors).size).toBe(anchors.length);
  });

  it("names the surface and the card it lands on", () => {
    // The gap this catalog closes: several controls promised "the MANVI view"
    // and landed the reader somewhere other than their subject. A hint that
    // does not name both halves of the destination repeats that.
    for (const target of MANVI_FOCUS_LIST) {
      const hint = manviFocusHint(target.id);
      expect(hint).toContain(target.destination.label);
      expect(hint).toContain(target.label);
    }
    expect(manviFocusHint("model")).toBe("Opens Settings → Agents → Local model servers.");
    expect(manviFocusHint("cleanup")).toBe("Opens Work → Overview → Branch and tag cleanup.");
  });

  it("anchors every catalogued target in the one component that owns its card", () => {
    // Derived from the catalog rather than a hand-written list: a target added
    // without its anchor fails here instead of silently landing the reader at
    // the top of the page.
    for (const target of MANVI_FOCUS_LIST) {
      const owner = componentSource(CARD_OWNER[target.id]);
      const anchor = `id={manviSectionId("${target.id}")}`;
      expect(owner, `${target.id}: anchor missing from ${CARD_OWNER[target.id]}`).toContain(anchor);
      // Focus follows the scroll, so keyboard users land on the same card.
      expect(owner.slice(owner.indexOf(anchor)).slice(0, 200)).toContain('tabindex="-1"');
    }
  });

  it("anchors each card exactly once across the components", () => {
    // Two copies of one card is the drift this split exists to prevent.
    for (const target of MANVI_FOCUS_LIST) {
      const anchor = `manviSectionId("${target.id}")`;
      const holders = Object.values(CARD_OWNER).filter((name) =>
        componentSource(name).includes(`id={${anchor}}`),
      );
      expect(holders, target.id).toEqual([CARD_OWNER[target.id]]);
    }
  });

  it("has a consumer for every destination kind, taking only its own kind", () => {
    for (const [kind, name] of Object.entries(CONSUMER)) {
      const source = componentSource(name);
      expect(source, name).toContain(`takeManviFocusFor("${kind}")`);
    }
  });

  it("retires the Policy panes, so nothing renders the old panel", () => {
    for (const retired of ["ManviOpsPanel.svelte", "ManviHarnessPane.svelte", "PolicyPane.svelte"]) {
      expect(existsSync(join(componentDir, retired)), retired).toBe(false);
    }
  });
});

describe("Manvi focus request channel", () => {
  beforeEach(() => {
    takeManviFocus();
  });

  it("hands one pending request to the surface that mounts after the click", () => {
    expect(get(manviFocusRequest)).toBeNull();
    requestManviFocus("model");
    expect(get(manviFocusRequest)).toBe("model");
    expect(takeManviFocus()).toBe("model");
  });

  it("clears on read so a later visit does not re-scroll", () => {
    requestManviFocus("activity");
    expect(takeManviFocus()).toBe("activity");
    expect(takeManviFocus()).toBeNull();
    expect(get(manviFocusRequest)).toBeNull();
  });

  it("keeps only the most recent request", () => {
    requestManviFocus("harness");
    requestManviFocus("cleanup");
    expect(takeManviFocus()).toBe("cleanup");
  });

  it("lets each surface take only the requests whose card it renders", () => {
    // Overview stays mounted under the Settings dialog. If it took every
    // request, a "model" link would be cleared before Settings mounted and
    // Settings would open on its first section.
    requestManviFocus("model");
    expect(takeManviFocusFor("view")).toBeNull();
    expect(get(manviFocusRequest)).toBe("model");
    expect(takeManviFocusFor("settings")).toBe("model");
    requestManviFocus("cleanup");
    expect(takeManviFocusFor("settings")).toBeNull();
    expect(takeManviFocusFor("view")).toBe("cleanup");
    expect(get(manviFocusRequest)).toBeNull();
  });
});
