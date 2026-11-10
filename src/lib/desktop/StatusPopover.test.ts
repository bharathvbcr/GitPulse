import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import StatusPopover from "./StatusPopover.svelte";
import { statusFixture } from "../../../harness/statusFixtures";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "StatusPopover.svelte"),
  "utf8",
);

describe("compact status popover", () => {
  it("matches the reference's three-card first glance and keeps secondary controls behind Details", () => {
    const { body } = render(StatusPopover, { props: { snapshot: statusFixture("changes"), onaction: () => {} } });
    expect(body.match(/class="metric\s/g)).toHaveLength(3);
    expect(body).toContain("fetched 4 min ago");
    expect(body).toContain('class="caption');
    expect(body).not.toContain('>last fetch<');
    expect(body).not.toContain('aria-label="2 stashes"');
    expect(body).not.toContain('aria-label="Go"');
    expect(body).not.toContain('aria-label="Tools"');
    expect(body).not.toContain('aria-label="Command palette"');
    expect(body).not.toContain("4 of 12 staged");
  });
  it("projects per-repo badges into the switcher fixture", () => {
    const snapshot = statusFixture("changes");
    expect(snapshot.repositories[1]).toMatchObject({ changed: 3, conflicts: 1, busy: true });
  });
  it("surfaces never-fetched upstream state", () => {
    const { body } = render(StatusPopover, { props: { snapshot: statusFixture("never fetched"), onaction: () => {} } });
    expect(body).toContain("never fetched");
  });
  it("shows review metrics and the primary action without expanding the details", () => {
    const { body } = render(StatusPopover, { props: { snapshot: statusFixture("changes"), onaction: () => {} } });
    expect(body).toContain('aria-label="12 changed"');
    expect(body).toContain('aria-label="4 staged"');
    expect(body).toContain('aria-label="0 conflicts"');
    expect(body).toContain("Review changes");
    expect(body).toContain("ahead");
    expect(body).not.toContain('id="status-details"');
    expect(body).not.toContain("listed stashes");
  });
  it("keeps missing values distinct from a clean working tree", () => {
    const { body } = render(StatusPopover, { props: { snapshot: statusFixture("unavailable"), onaction: () => {} } });
    expect(body).toContain('aria-label="Unknown changed"');
    expect(body).toContain("Status unavailable");
    expect(body).toContain("Try again");
    expect(body).not.toContain("All changes committed");
  });
  it("offers a short first-open state instead of empty metric cards", () => {
    const { body } = render(StatusPopover, { props: { snapshot: statusFixture("empty"), onaction: () => {} } });
    expect(body).toContain("Open repository");
    expect(body).toContain("Clone repository");
    expect(body).not.toContain('aria-label="Working tree counts"');
  });
  it("surfaces urgent work in the headline while keeping shortcuts in the disclosure", () => {
    const busy = render(StatusPopover, { props: { snapshot: statusFixture("busy"), onaction: () => {} } }).body;
    expect(busy).toContain("Fetching…");
    expect(busy).not.toContain('aria-label="Workspace insights"');
    expect(busy).not.toContain("Push");
    expect(busy).not.toContain("Quick Commit");
    const parked = render(StatusPopover, { props: { snapshot: statusFixture("operation"), onaction: () => {} } }).body;
    expect(parked).toContain("Merge in progress — on main");
    expect(parked).not.toContain('aria-label="Command palette"');
  });
  it("uses the app's liquid blur glass in the native material mode", () => {
    // The material block should contain the hue field, glass fills, specular
    // edge treatment, and liquid-ease transitions so the popover reads as the
    // same glass surface language as the main workspace.
    const nativeBlock = source.slice(
      source.indexOf('[data-material="native"]'),
      source.indexOf("@supports"),
    );
    // Hue field (radial-gradient blobs matching .gp-shell in app.css).
    expect(nativeBlock).toContain("radial-gradient");
    expect(nativeBlock).toContain("--hue-a");
    expect(nativeBlock).toContain("--hue-gain");
    // Specular glass edge treatment (inset sheen + shade matching gp-glass).
    expect(nativeBlock).toContain("--glass-sheen-top");
    expect(nativeBlock).toContain("--glass-shade-bottom");
    expect(nativeBlock).toContain("inset 0 1px 0 var(--glass-sheen-top)");
    expect(nativeBlock).toContain("inset 0 -1px 0 var(--glass-shade-bottom)");
    // The panel alpha is a contrast floor, not a style knob, and this
    // assertion exists to stop it drifting down again. It read `.55` when the
    // glass landed — chosen as "not the old opaque .8" — which put the accent
    // tokens at 2.1-2.5:1 over a white or black desktop, against the 4.5:1
    // this popover has to hold anywhere it is opened. Solved against the model
    // in harness/statusChecks.ts, the minimum is .787 light / .795 dark, so .8
    // is the lowest round value that clears both. The glass is unaffected: the
    // hue field, sheen and tint above all still paint, and a fifth of the
    // desktop still shows through.
    expect(nativeBlock).toContain("--bg:rgb(var(--base) / .8)");
    // The inner cards sit ON the panel rather than on the desktop, so they are
    // free to stay thin — their backdrop is already the panel above.
    expect(nativeBlock).toContain("--surface:rgb(var(--card-base) / .4)");
    // Liquid-ease cubic-bezier on interactive elements.
    expect(nativeBlock).toContain("--liquid-ease:cubic-bezier(0.22, 1, 0.36, 1)");
    expect(nativeBlock).toContain("scale(0.97)");
  });
  it("tunes the browser fixture from the same rules as the native material", () => {
    // The fixture used to carry its own copy of the glass ladder, and the copy
    // drifted: it had the base tuning but neither appearance arm, so in light
    // mode it drew the dark glass — a .05 sheen against the material's .7, a
    // black .22 shade against its .07 navy, .4 cards against .58, and the dark
    // hue blobs at twice the gain. Nothing caught it, because a fixture is
    // only ever compared against itself.
    //
    // So the invariant is structural rather than a list of values: every rule
    // that declares or reads a glass token has to name both materials — the
    // ladder, both appearance arms, the panel treatment and the inner cards,
    // five of them as this stands. Comments come out first: prose about one
    // material must not stand in for a selector that targets it. The blur is
    // the sole material-specific rule and touches no glass token, because
    // native's blur comes from the window server rather than from CSS.
    const styles = source.slice(source.indexOf("<style>")).replace(/\/\*[\s\S]*?\*\//g, "");
    const tuned = [...styles.matchAll(/([^{}]+)\{([^}]*)\}/g)]
      .filter(([, selector, body]) => body.includes("--glass-") && selector.includes("data-material"))
      .map(([, selector]) => selector.replace(/\s+/g, " ").trim());
    expect(tuned.length).toBeGreaterThanOrEqual(3);
    for (const selector of tuned) {
      expect(selector).toContain('[data-material="native"]');
      expect(selector).toContain('[data-material="preview"]');
    }
  });
});

describe("reduced transparency in the status popover", () => {
  // WKWebView does not implement prefers-reduced-transparency, so the media
  // query alone never fires for the macOS setting. The attribute selector is
  // what the native bridge reaches; the two must say the same thing.
  const styles = source.slice(source.indexOf("<style>")).replace(/\/\*[\s\S]*?\*\//g, "");
  const normalize = (text: string) => text.replace(/\s+/g, " ").trim();

  function mediaBody(): string {
    const start = styles.indexOf("@media (prefers-reduced-transparency: reduce)");
    expect(start).toBeGreaterThan(-1);
    const open = styles.indexOf("{", start);
    let depth = 0;
    for (let i = open; i < styles.length; i += 1) {
      if (styles[i] === "{") depth += 1;
      if (styles[i] === "}" && --depth === 0) return styles.slice(open + 1, i);
    }
    throw new Error("unterminated reduced-transparency block");
  }

  it("keeps the media query beside the attribute", () => {
    expect(styles).toContain("@media (prefers-reduced-transparency: reduce), (prefers-contrast: more), (forced-colors: active)");
  });

  it("restyles under html[data-transparency=reduced] with the media block's declarations", () => {
    const attribute = [...styles.matchAll(/:global\(html\[data-transparency="reduced"\]\)\s*([^{}]+)\{([^}]*)\}/g)]
      .map(([, selector, body]) => [normalize(selector), normalize(body)]);
    const media = [...mediaBody().matchAll(/([^{}]+)\{([^}]*)\}/g)]
      .map(([, selector, body]) => [normalize(selector), normalize(body)]);
    expect(media).toHaveLength(3);
    // The shell and the panel. The media block's third rule (inner cards) is
    // already satisfied by the glass rule through --surface and --line, and
    // repeating it would only add scoped widget-dialect selectors (rule 12).
    expect(attribute).toEqual(media.slice(0, 2));
    const [, , cards] = media;
    expect(cards[1]).toBe("background:var(--surface); border-color:var(--line);");
    expect(styles).toMatch(/--line:var\(--glass-edge\)/);
  });

  it("does not react to the unknown state", () => {
    expect(styles).not.toContain('data-transparency="unknown"');
  });
});
