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

  it("draws Manvi's published Fold M mark with brand plate, palette, and unique gradient IDs", () => {
    const source = readFileSync(new URL("../components/BrandMark.svelte", import.meta.url), "utf8");
    const outline = /const MANVI_FOLD_OUTLINE =\s*"([^"]+)";/.exec(source)?.[1] ?? "";
    const lit = /const MANVI_FOLD_LIT =\s*"([^"]+)";/.exec(source)?.[1] ?? "";
    expect(outline).toBe(
      "M58 223L100 97L136 97L160 169L184 97L220 97L262 223L226 223L202 151L178 223L142 223L118 151L94 223Z",
    );
    expect(lit).toBe("M58 223L94 223L136 97L100 97ZM142 223L178 223L220 97L184 97Z");
    expect(source).toContain('viewBox="0 0 320 320"');
    expect(source).toContain('plateTop: "#24131b"');
    expect(source).toContain('plateBottom: "#0c090c"');
    expect(source).toContain('glow: "#d92d3f"');
    expect(source).toContain('litTop: "#f0959e"');
    expect(source).toContain('litMid: "#e0616f"');
    expect(source).toContain('litBottom: "#d92d3f"');
    expect(source).toContain('shadeTop: "#b02636"');
    expect(source).toContain('shadeBottom: "#7a1626"');
    expect(source).toContain("const uid = $props.id();");
    for (const id of ["plate", "glow", "lit", "shade"]) {
      expect(source).toContain(`id={\`\${uid}-manvi-${id}\`}`);
      expect(source).toContain(`url(#\${uid}-manvi-${id})`);
    }
  });

  it("falls back to the shell glyph for an unknown or missing kind", () => {
    expect(launcherIcon(undefined)).toBe(LAUNCHER_ICONS.shell);
    expect(launcherIcon("nope" as never)).toBe(LAUNCHER_ICONS.shell);
  });
});
