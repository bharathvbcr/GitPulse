import { readdirSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { compile } from "svelte/compiler";
import { render as ssr } from "svelte/server";
import ScrollCue from "./ScrollCue.svelte";

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, "ScrollCue.svelte"), "utf8");

describe("ScrollCue", () => {
  it("compiles", () => {
    const result = compile(source, { filename: "ScrollCue.svelte", generate: "server" });
    expect(result.js.code.length).toBeGreaterThan(0);
  });

  it("advertises overflow with a chevron that is not in the tab order", () => {
    expect(source).toContain("ChevronLeft");
    expect(source).toContain("ChevronRight");
    expect(source).toContain("ChevronUp");
    expect(source).toContain("ChevronDown");
    expect(source).toContain("observeOverflow");
    expect(source).toContain("scrollOverflowBy");
    expect(source).toContain('tabindex="-1"');
    expect(source).toContain("prefersReducedMotion");
    expect(source).toContain('aria-label={label}');
  });

  it("lets clicks through the overlay and only captures the chevron", () => {
    expect(source).toContain("gp-scroll-cue");
    expect(source).toContain("gp-scroll-cue-y");
    expect(source).toContain("gp-scroll-cue-btn");
    expect(source).toContain("e.stopPropagation()");
    expect(source).toContain("e.preventDefault()");
  });
});

describe("ScrollCue diffuse mode", () => {
  const css = readFileSync(join(here, "../../app.css"), "utf8");
  const block = css.slice(
    css.indexOf("/* Header lane I: diffuse scroll edge */"),
    css.indexOf("@utility gp-scroll-cue-btn"),
  );

  // A real server render: the component's own markup, not a source grep.
  function render(props: Record<string, unknown>): string {
    const target = {
      parentElement: null,
      addEventListener: () => {},
      removeEventListener: () => {},
    } as unknown as HTMLElement;
    return ssr(ScrollCue, { props: { target, axis: "y", ...props } }).body;
  }

  const both = { canScroll: true, showStart: true, showEnd: true };
  const none = { canScroll: false, showStart: false, showEnd: false };

  it("default render has no strips and no hover-only chevrons", () => {
    const html = render({ hint: both });
    expect(html).not.toContain("gp-scroll-diffuse");
    expect(html).not.toContain("data-hover-only");
    expect(html).toContain('aria-label="Scroll up"');
    expect(html).toContain('aria-label="Scroll down"');
  });

  it("diffuse renders two aria-hidden strips on each edge that hides content", () => {
    const html = render({ hint: both, diffuse: true });
    expect(html.match(/class="gp-scroll-diffuse"/g)).toHaveLength(2);
    expect(html.match(/gp-scroll-diffuse-strip/g)).toHaveLength(4);
    expect(html).toContain('data-edge="top"');
    expect(html).toContain('data-edge="bottom"');
    expect(html.match(/class="gp-scroll-diffuse" data-edge="\w+" aria-hidden="true"/g)).toHaveLength(2);
    expect(html).toContain("data-hover-only");
    expect(html).toContain('aria-label="Scroll up"');
  });

  it("diffuse strips follow showStart and showEnd per edge, on the x axis too", () => {
    const html = render({
      hint: { canScroll: true, showStart: false, showEnd: true },
      diffuse: true,
      axis: "x",
    });
    expect(html).toContain('data-edge="right"');
    expect(html).not.toContain('data-edge="left"');
  });

  it("renders no strips when nothing scrolls", () => {
    const html = render({ hint: none, diffuse: true });
    expect(html).not.toContain("gp-scroll-diffuse");
  });

  it("fades strips in at 140 ms and out at 70 ms through the shared helpers", () => {
    expect(source).toContain("backdropFade()");
    expect(source).toContain("backdropFadeOut()");
    expect(source).not.toMatch(/duration:\s*\d/);
  });

  it("app.css block blurs in proportion to --gp-float-blur and masks inward", () => {
    expect(block).toContain("calc(var(--gp-float-blur, 34px) * 0.2)");
    expect(block).toContain("calc(var(--gp-float-blur, 34px) * 0.5)");
    expect(block).toMatch(/mask-image: linear-gradient\(var\(--gp-diffuse-dir\), #000, transparent\)/);
    expect(block).toContain("backdrop-filter: blur(var(--gp-diffuse-light))");
    expect(block).toContain("pointer-events-none");
    expect(block).not.toContain("transition: all");
  });

  it("app.css block renders nothing when float blur is off or transparency is reduced", () => {
    expect(block).toMatch(
      /html\[data-float-blur="off"\] \.gp-scroll-diffuse,\s*html\[data-transparency~="reduced"\] \.gp-scroll-diffuse \{\s*display: none;/,
    );
    // At the end of app.css, outside the block above, by design.
    expect(css).toMatch(/@media \(prefers-contrast: more\), \(forced-colors: active\), \(prefers-reduced-transparency: reduce\) \{\s*\.gp-scroll-diffuse \{\s*display: none;/);
  });

  it("hover-only chevron fade is in both reduced-motion lists", () => {
    expect(css.match(/\.gp-md-copy, \.gp-scroll-cue-btn\) \{/g)).toHaveLength(2);
  });
});

/**
 * The fade used to be painted: a 32px band of one colour over whatever the
 * host was. No single colour matches a glass title bar, a surface toolbar
 * and a base pane at once, so on every host but one the band read as a
 * blank patch with a hard edge, sitting there with nothing hovered. The
 * scroller now fades its own content with a mask, which needs no colour.
 */
describe("ScrollCue fades the scroller, never paints over it", () => {
  const css = readFileSync(join(here, "../../app.css"), "utf8");

  function utility(name: string): string {
    const start = css.indexOf(`@utility ${name} {`);
    expect(start, `@utility ${name}`).toBeGreaterThanOrEqual(0);
    return css.slice(start, css.indexOf("\n}", start));
  }

  it("marks the target with the edges that still hide content", () => {
    expect(source).toContain("overflowFadeToken");
    expect(source).toContain("setOverflowFade");
  });

  it.each(["x-start", "x-end", "x-both", "y-start", "y-end", "y-both"])(
    "app.css masks a %s scroller",
    (token) => {
      const at = css.indexOf(`[data-gp-fade="${token}"]`);
      expect(at, token).toBeGreaterThanOrEqual(0);
    },
  );

  it("applies the mask with and without the WebKit prefix", () => {
    const rule = css.slice(css.indexOf("[data-gp-fade] {"));
    expect(rule).toMatch(/^\[data-gp-fade\] \{[^}]*-webkit-mask-image: var\(--gp-fade-mask\)/);
    expect(rule).toMatch(/^\[data-gp-fade\] \{[^}]*\n\s*mask-image: var\(--gp-fade-mask\)/);
  });

  it.each(["gp-scroll-cue", "gp-scroll-cue-y", "gp-scroll-cue-start", "gp-scroll-cue-end", "gp-scroll-cue-top", "gp-scroll-cue-bottom"])(
    "the %s overlay paints no background",
    (name) => {
      expect(utility(name)).not.toMatch(/background|bg-/);
    },
  );

  it("leaves no painted edge fade or per-platform shade behind", () => {
    expect(css).not.toContain("gp-edge-fade");
    expect(css).not.toContain("--mac-edge-shade");
  });

  it("no component paints the retired fade classes directly", () => {
    const offenders: string[] = [];
    const walk = (dir: string) => {
      for (const entry of readdirSync(dir, { withFileTypes: true })) {
        const path = join(dir, entry.name);
        if (entry.isDirectory()) walk(path);
        else if (/\.(svelte|ts|html)$/.test(entry.name) && !/\.test\.ts$/.test(entry.name)
          && readFileSync(path, "utf8").includes("gp-edge-fade")) offenders.push(path);
      }
    };
    walk(join(here, "../.."));
    expect(offenders).toEqual([]);
  });
});
