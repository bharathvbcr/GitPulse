/**
 * The dock closes on a pointerdown outside it, except on its own trigger. The
 * trigger is now the terminal pill in the view bar's second row, so the
 * exemption selector TerminalDock passes must name attributes the pill really
 * renders, and the pill must be mounted by the view bar (not left behind in
 * the status bar). A pill that stopped matching would close the dock on
 * pointerdown and reopen it on click.
 */
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const dir = dirname(fileURLToPath(import.meta.url));
const read = (name: string) => readFileSync(join(dir, name), "utf8");

describe("terminal dock trigger exemption", () => {
  const dock = read("TerminalDock.svelte");
  const pill = read("TerminalPill.svelte");

  it("exempts the selectors the pill renders", () => {
    // One constant serves both the outside-click dismiss and the double-click
    // hide, so the pill cannot be exempt from one and not the other.
    const match = dock.match(/const DOCK_TRIGGER = "([^"]+)";/);
    expect(match).not.toBeNull();
    expect(dock).toContain('floatingPanelDismiss("[data-terminal-dock]", DOCK_TRIGGER)');
    expect(dock).toContain("trigger: DOCK_TRIGGER");
    const selectors = (match?.[1] ?? "").split(",").map((s) => s.trim());
    expect(selectors.length).toBeGreaterThan(0);
    for (const selector of selectors) {
      const attribute = selector.replace(/^\[/, "").replace(/\]$/, "");
      expect(pill).toContain(attribute);
    }
    expect(selectors).toContain("[data-terminal-pill]");
  });

  it("puts the pill's toggle inside the exempt wrapper", () => {
    const wrapper = pill.indexOf("data-terminal-pill\n");
    const toggle = pill.indexOf('data-testid="terminal-pill"');
    expect(wrapper).toBeGreaterThan(-1);
    expect(toggle).toBeGreaterThan(wrapper);
    expect(pill).toContain("onclick={toggleDock}");
  });

  it("mounts the pill in the view bar and not in the status bar", () => {
    expect(read("ViewBar.svelte")).toContain("<TerminalPill />");
    expect(read("StatusBar.svelte")).not.toContain("<TerminalPill");
  });
});
