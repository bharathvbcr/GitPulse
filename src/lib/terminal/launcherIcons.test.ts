import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { LAUNCHER_ICONS, launcherIcon } from "./launcherIcons";
import { SESSION_TYPES } from "./tabs";

describe("launcher icons", () => {
  it("draws every session kind, and the file pane, with a glyph or a mark", () => {
    for (const kind of [...SESSION_TYPES.map((type) => type.kind), "file" as const]) {
      const spec = LAUNCHER_ICONS[kind];
      expect(spec.icon !== null || spec.mark !== undefined, kind).toBe(true);
      expect(spec.tint.length, kind).toBeGreaterThan(0);
    }
  });

  it("draws each agent CLI with its vendor's own mark", () => {
    expect(LAUNCHER_ICONS.claude).toMatchObject({ icon: null, mark: "claude", tint: "#d97757" });
    expect(LAUNCHER_ICONS.codex).toMatchObject({ icon: null, mark: "codex" });
    expect(LAUNCHER_ICONS.grok).toMatchObject({ icon: null, mark: "grok" });
    expect(LAUNCHER_ICONS.agy).toMatchObject({ icon: null, mark: "agy" });
  });

  it("draws a plain shell with GitPulse's own mark, in the muted text colour", () => {
    expect(LAUNCHER_ICONS.shell).toEqual({ icon: null, mark: "shell", tint: "var(--color-textMuted)" });
  });

  it("draws a Manvi session with Manvi's own mark in Manvi's red, not a generic shield", () => {
    expect(LAUNCHER_ICONS.manvi).toEqual({ icon: null, mark: "manvi", tint: "#d92d3f" });
  });

  it("names only marks BrandMark.svelte can draw, each as one filled path", () => {
    const source = readFileSync(new URL("../components/BrandMark.svelte", import.meta.url), "utf8");
    const table = /const MARKS[^=]*=\s*\{([\s\S]*?)\n {2}\};/.exec(source)?.[1] ?? "";
    const drawn = new Map([...table.matchAll(/^ {4}(\w+): \{[\s\S]*?\bd: "([^"]+)"/gm)].map((m) => [m[1], m[2]]));
    expect(drawn.size).toBeGreaterThan(0);
    for (const [kind, spec] of Object.entries(LAUNCHER_ICONS)) {
      // Manvi's is its published two-colour tile, checked on its own below.
      if (!spec.mark || spec.mark === "manvi") continue;
      const d = drawn.get(spec.mark);
      expect(d, `${kind} names the mark ${spec.mark}`).toBeDefined();
      // One path in the caller's colour: no second `d`, nothing a renderer
      // would have to resolve by id.
      expect(d, spec.mark).toMatch(/^[Mm][\d\s.,a-zA-Z-]+$/);
    }
  });

  it("draws Manvi's published mark: its 7x7 grid M on the Red700 tile, in Red050", () => {
    // Manvi's own grid (manvi/ui/logo/logo.go), from which svg.go renders
    // assets/manvi-mark.svg; '#' is the M knocked out of the tile.
    const grid = [".......", ".#...#.", ".##.##.", ".#.#.#.", ".#...#.", ".#...#.", "......."];
    const source = readFileSync(new URL("../components/BrandMark.svelte", import.meta.url), "utf8");
    const pathSource = /const MANVI_M =([\s\S]*?);/.exec(source)?.[1] ?? "";
    const d = [...pathSource.matchAll(/"([^"]*)"/g)].map((m) => m[1]).join("");
    const drawn = Array.from({ length: 7 }, () => Array<string>(7).fill("."));
    for (const [, x, y, w, h] of d.matchAll(/M(\d+) (\d+)h(\d+)v(\d+)h-\d+z/g)) {
      for (let row = Number(y) / 12; row < (Number(y) + Number(h)) / 12; row++) {
        for (let col = Number(x) / 12; col < (Number(x) + Number(w)) / 12; col++) drawn[row][col] = "#";
      }
    }
    expect(d.length).toBeGreaterThan(0);
    expect(drawn.map((row) => row.join(""))).toEqual(grid);
    expect(source).toMatch(/const MANVI_TILE = "#7a1626";/);
    expect(source).toMatch(/const MANVI_GLYPH = "#fdf2f3";/);
  });

  it("falls back to the shell glyph for an unknown or missing kind", () => {
    expect(launcherIcon(undefined)).toBe(LAUNCHER_ICONS.shell);
    expect(launcherIcon("nope" as never)).toBe(LAUNCHER_ICONS.shell);
  });
});
