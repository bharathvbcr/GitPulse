import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { beforeEach, describe, expect, it } from "vitest";
import { get } from "svelte/store";
import {
  MANVI_FOCUS_IDS,
  MANVI_FOCUS_LIST,
  MANVI_FOCUS_TARGETS,
  MANVI_PANES,
  MANVI_PANE_IDS,
  manviFocusHint,
  manviFocusRequest,
  manviSectionId,
  requestManviFocus,
  takeManviFocus,
} from "./manviFocus";

const componentDir = join(
  dirname(fileURLToPath(import.meta.url)),
  "..",
  "components",
);

function componentSource(name: string): string {
  return readFileSync(join(componentDir, name), "utf8");
}

/** The file that renders each pane's sections. */
const PANE_SOURCE: Record<string, string> = {
  ops: componentSource("ManviOpsPanel.svelte"),
  harness: componentSource("ManviHarnessPane.svelte"),
};

describe("MANVI focus catalog", () => {
  it("registers every target against a known pane, keyed by its own id", () => {
    for (const id of MANVI_FOCUS_IDS) {
      const target = MANVI_FOCUS_TARGETS[id];
      expect(target.id).toBe(id);
      expect(MANVI_PANE_IDS).toContain(target.pane);
      expect(target.label.length).toBeGreaterThan(0);
    }
    expect(MANVI_FOCUS_LIST).toHaveLength(MANVI_FOCUS_IDS.length);
  });

  it("derives a distinct DOM anchor per target", () => {
    const anchors = MANVI_FOCUS_LIST.map((target) => manviSectionId(target.id));
    expect(new Set(anchors).size).toBe(anchors.length);
  });

  it("names the pane and the section it lands on", () => {
    // The gap this catalog closes: several controls promised "the MANVI view"
    // and landed the reader on a different pane from their subject. A hint
    // that does not name both halves of the destination repeats that.
    for (const target of MANVI_FOCUS_LIST) {
      const hint = manviFocusHint(target.id);
      expect(hint).toContain(MANVI_PANES[target.pane].label);
      expect(hint).toContain(target.label);
    }
  });

  it("anchors every catalogued target in the pane that owns it", () => {
    // Derived from the catalog rather than a hand-written list: a target added
    // without its section anchor fails here instead of silently landing the
    // reader at the top of the page.
    for (const target of MANVI_FOCUS_LIST) {
      const owner = PANE_SOURCE[target.pane];
      expect(
        owner,
        `${target.id}: anchor missing from the ${target.pane} pane`,
      ).toContain(`id={manviSectionId("${target.id}")}`);
      // Focus follows the scroll, so keyboard users land on the same card.
      const anchored = owner.slice(
        owner.indexOf(`id={manviSectionId("${target.id}")}`),
      );
      expect(anchored.slice(0, 200)).toContain('tabindex="-1"');
    }
  });

  it("keeps the pane labels the tooltips promise in one place", () => {
    const panel = PANE_SOURCE.ops;
    expect(panel).toContain("MANVI_PANE_LIST as entry");
    for (const pane of MANVI_PANE_IDS) {
      // The literal label must not be re-typed beside the catalog entry.
      expect(panel).not.toContain(`>${MANVI_PANES[pane].label}<`);
    }
  });
});

describe("MANVI focus request channel", () => {
  beforeEach(() => {
    takeManviFocus();
  });

  it("hands one pending request to the view that mounts after the click", () => {
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
});
