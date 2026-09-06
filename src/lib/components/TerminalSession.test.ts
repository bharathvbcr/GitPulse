import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { hexColor, shouldRefit } from "./TerminalSession.svelte";

/**
 * These contracts moved here with the PTY itself when tabs arrived: they were
 * written against TerminalPanel when it owned the single session, and they
 * describe session behaviour, not strip behaviour.
 */
const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "TerminalSession.svelte"),
  "utf8",
);

describe("TerminalSession refit guard", () => {
  const dims = (cols: number, rows: number) => ({ cols, rows });

  it("skips the fit when the proposed grid equals the live one", () => {
    expect(shouldRefit(dims(80, 24), dims(80, 24))).toBe(false);
  });

  it("refits when cols or rows actually change", () => {
    expect(shouldRefit(dims(80, 24), dims(81, 24))).toBe(true);
    expect(shouldRefit(dims(80, 24), dims(80, 25))).toBe(true);
  });

  it("skips on an unusable proposal instead of guessing a grid", () => {
    // Hidden panels and mid-layout containers make proposeDimensions()
    // return undefined; fitting then would throw or no-op with churn.
    expect(shouldRefit(dims(80, 24), null)).toBe(false);
    expect(shouldRefit(dims(80, 24), undefined)).toBe(false);
    expect(shouldRefit(dims(80, 24), dims(Number.NaN, 24))).toBe(false);
    expect(shouldRefit(dims(80, 24), dims(80, Number.POSITIVE_INFINITY))).toBe(false);
  });

  it("never resizes a hidden tab down to a degenerate grid", () => {
    // An inactive tab is display:none, so its ResizeObserver fires with a
    // zero rect on every strip change. Acting on that would tell the shell
    // its window is 1x1 and reflow the buffer for a tab nobody is looking at.
    expect(shouldRefit(dims(80, 24), dims(0, 0))).toBe(false);
    expect(shouldRefit(null, dims(0, 24))).toBe(false);
    expect(shouldRefit(dims(80, 24), dims(0.4, 24))).toBe(false);
  });

  it("refits once when there is no baseline yet but the proposal is usable", () => {
    expect(shouldRefit(null, dims(80, 24))).toBe(true);
    expect(shouldRefit(null, undefined)).toBe(false);
  });

  it("treats sub-pixel float noise as unchanged", () => {
    expect(shouldRefit(dims(80, 24), dims(80.4, 23.6))).toBe(false);
    expect(shouldRefit(dims(80, 24), dims(80.6, 24.4))).toBe(true);
  });
});

describe("TerminalSession PTY contracts", () => {
  it("invokes every PTY session endpoint", () => {
    expect(source).toContain('"cmd_terminal_spawn"');
    expect(source).toContain('"cmd_terminal_write"');
    expect(source).toContain('"cmd_terminal_resize"');
    expect(source).toContain('"cmd_terminal_kill"');
  });

  it("constructs exactly one XTerm per session", () => {
    expect(source.match(/new XTerm\(/g)?.length).toBe(1);
  });

  it("decouples creation from attachment: ensureTerm never opens a container", () => {
    const ensureBody = source.slice(
      source.indexOf("function ensureTerm"),
      source.indexOf("function refitIfResized"),
    );
    expect(ensureBody).not.toContain(".open(");
  });

  it("guards ResizeObserver refits with proposeDimensions + shouldRefit", () => {
    const refitBody = source.slice(
      source.indexOf("function refitIfResized"),
      source.indexOf("function base64ToBytes"),
    );
    expect(refitBody).toContain("proposeDimensions()");
    expect(refitBody).toContain("shouldRefit(");
    expect(refitBody).toContain("fitAddon.fit()");
  });

  it("keeps the resize IPC wired to real dimension changes via onResize", () => {
    const onResizeBody = source.slice(
      source.indexOf("created.onResize("),
      source.indexOf("created.onTitleChange("),
    );
    expect(onResizeBody).toContain('"cmd_terminal_resize"');
  });

  it("applies the theme palette reactively from themeStore", () => {
    expect(source).toContain('import { themeStore } from "../stores/themeStore";');
    const effectIdx = source.indexOf("$themeStore;");
    expect(effectIdx).toBeGreaterThan(-1);
    expect(source.indexOf("term.options.theme = termTheme()", effectIdx)).toBeGreaterThan(effectIdx);
    // Construction-time palette stays too: a term created between emissions
    // still gets the current CSS-variable palette immediately.
    expect(source.slice(0, effectIdx)).toContain("theme: termTheme()");
  });

  it("reserves one fixed-height status row instead of swapping py-2/py-1.5 strips", () => {
    expect(source).not.toContain("py-2 border-t");
    expect(source).not.toContain("py-1.5 border-t");
    expect(source).toContain("px-4 h-8");
  });

  it("journals the session against the repository that started it", () => {
    expect(source).toMatch(/harnessStore\.recordAction\(\{\s+repoPath,/);
    expect(source).toContain("not gate-checked");
  });

  it("kills a session whose spawn landed after the component went away", () => {
    // Teardown can run while cmd_terminal_spawn is pending; without this the
    // late response adopts a backend session with no owner.
    const disposedIdx = source.indexOf("if (disposed) {");
    expect(disposedIdx).toBeGreaterThan(-1);
    expect(source.indexOf("void killPty(spawned.id);", disposedIdx)).toBeGreaterThan(disposedIdx);
    // …and the flag is set as the first act of cleanup, before the kill.
    const cleanupIdx = source.indexOf("disposed = true;");
    expect(cleanupIdx).toBeGreaterThan(-1);
    expect(source.indexOf("void killPty(sessionId);", cleanupIdx)).toBeGreaterThan(cleanupIdx);
  });

  it("releases its bus subscription on teardown, not just its session", () => {
    const cleanupIdx = source.indexOf("disposed = true;");
    expect(source.indexOf("unsubscribe?.();", cleanupIdx)).toBeGreaterThan(cleanupIdx);
  });

  it("spawns from mount, never from an effect that can re-run", () => {
    // repoStore republishes a fresh object every ~6s status poll, and the
    // panel's old spawn-in-an-effect had to be memoised against that or it
    // would kill and respawn the user's live shell per emission. Mount cannot
    // see those emissions at all — but only while no effect spawns.
    const mountIdx = source.indexOf("onMount(() => {");
    expect(mountIdx).toBeGreaterThan(-1);
    const mountBody = source.slice(mountIdx, source.indexOf("$effect(", mountIdx));
    expect(mountBody).toContain("void spawnPty();");

    // Exactly two call sites, and the other is the explicit Retry/Restart.
    expect(source.match(/void spawnPty\(\)/g)?.length).toBe(2);
    const restartBody = source.slice(
      source.indexOf("export function restart()"),
      source.indexOf("export function reveal()"),
    );
    expect(restartBody).toContain("void spawnPty();");

    // No effect body reaches spawnPty.
    for (const body of source.split("$effect(").slice(1)) {
      expect(body.slice(0, body.indexOf("});"))).not.toContain("spawnPty");
    }
  });

  it("lets the strip claim tab chords before xterm writes them to the shell", () => {
    // Without this the chord is typed into the shell, because the terminal is
    // exactly where it will always be typed.
    const handlerIdx = source.indexOf("created.attachCustomKeyEventHandler(");
    expect(handlerIdx).toBeGreaterThan(-1);
    const body = source.slice(handlerIdx, source.indexOf("term = created;"));
    expect(body).toContain('event.type !== "keydown"');
    expect(body).toContain("return !onChord(event);");
  });
});

/**
 * xterm parses `#rgb`/`#rgba`/`#rrggbb`/`#rrggbbaa` itself and pushes anything
 * else through a canvas probe that throws when the sampled alpha is not 255
 * (`css.toColor`, @xterm/xterm 6.0.0). `getComputedStyle` returns a
 * translucent token as `rgba(...)`, which is exactly the form that throws — so
 * a terminal on a glass surface depends on this conversion, not on taste.
 */
describe("TerminalSession surface colour", () => {
  it("re-spells a translucent computed colour as hex xterm can parse", () => {
    expect(hexColor("rgba(20, 26, 41, 0.5)")).toBe("#141a2980");
    expect(hexColor("rgb(20 26 41 / 0.5)")).toBe("#141a2980");
  });

  it("drops the alpha byte when the surface is opaque", () => {
    // `#rrggbb` and `#rrggbbaa` are both accepted; the shorter one keeps the
    // opaque case identical to what the terminal was given before.
    expect(hexColor("rgb(20, 26, 41)")).toBe("#141a29");
    expect(hexColor("rgba(20, 26, 41, 1)")).toBe("#141a29");
  });

  it("spells the transparent keyword as hex too", () => {
    // `transparent` is a keyword rather than a function, and it takes the same
    // canvas path — where it samples alpha 0 and throws exactly as a
    // translucent `rgba()` does.
    expect(hexColor("transparent")).toBe("#00000000");
  });

  it("passes through anything it cannot read rather than guessing", () => {
    // xterm's parser handles far more than this does; mangling a colour it
    // would have understood is worse than handing it over untouched.
    expect(hexColor("#141a29")).toBe("#141a29");
    expect(hexColor("color-mix(in srgb, red, blue)")).toBe("color-mix(in srgb, red, blue)");
    expect(hexColor("rgb(20, 26)")).toBe("rgb(20, 26)");
  });

  it("reads the terminal's own token, and asks for transparency support", () => {
    // `--bg-surface` has a second runtime reader — the graph's node stroke —
    // and a stroke is not a fill. `allowTransparency` must be set before
    // `open()`, so it belongs in the constructor options.
    expect(source).toContain('hexColor(v("--bg-terminal"');
    expect(source).toContain("allowTransparency: true");
    // The block cursor's glyph colour is a foreground and stays opaque.
    expect(source).toContain('cursorAccent: v("--bg-surface"');
  });
});
