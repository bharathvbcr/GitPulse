import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, describe, expect, it } from "vitest";
import { render } from "svelte/server";
import StatusBar from "./StatusBar.svelte";
import { interfaceStore } from "../stores/interfaceStore";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "StatusBar.svelte"),
  "utf8",
);

const bar = () => render(StatusBar).body;

describe("StatusBar", () => {
  it("opens uncommitted files rather than retaining a previous commit diff", () => {
    const changes = source.slice(source.indexOf("<!-- Working Tree Changes"), source.indexOf("<!-- Conflicts Indicator"));
    expect(changes).toContain("repoStore.previewUncommitted()");
  });
  afterEach(() => interfaceStore.setStatusBarMode("full"));

  it("renders status bar role and shortcut indicators", () => {
    const body = bar();
    expect(body).toContain('role="status"');
    expect(body).toContain('aria-label="Repository Status Bar"');
    expect(body).toContain("Palette");
    expect(body).toContain("Shortcuts");
  });

  it("keeps the branch but drops the ambient readouts in compact mode", () => {
    interfaceStore.setStatusBarMode("minimal");
    const body = bar();
    expect(body).toContain('aria-label="Repository Status Bar"');
    expect(body).toContain("HEAD");
    // "Clean" and the shortcut hints say nothing is wrong, which is exactly
    // the noise a compact bar exists to lose.
    expect(body).not.toContain("Clean");
    expect(body).not.toContain("Palette");
    expect(body).not.toContain("Shortcuts");
  });

  it("renders nothing at all when hidden and the repository is quiet", () => {
    interfaceStore.setStatusBarMode("hidden");
    expect(bar()).not.toContain('role="status"');
  });

  it("uses the same liquid-blur chrome as the title bar, not a covering slab", () => {
    // Native under-window blur is the macOS material. Chrome shows it by
    // staying translucent (`gp-glass` + bare `bg-surface`). `bg-surface/95`
    // keeps its author alpha and paints over that material; `gp-gpu` is
    // `translateZ(0)`, which promotes a WKWebView layer that no longer
    // composites with the NSVisualEffectView behind it. The title bar has
    // neither, which is why it reads as glass and this strip did not.
    const footer = source.match(/<footer[\s\S]*?class="([^"]+)"/)?.[1];
    const tokens = footer?.split(/\s+/) ?? [];
    expect(tokens).toContain("gp-glass");
    expect(tokens).toContain("bg-surface");
    expect(tokens.some((token) => token.startsWith("bg-surface/"))).toBe(false);
    expect(tokens).not.toContain("gp-gpu");
  });

  it("feeds all three override signals to the visibility rule", () => {
    // The rule itself is covered in ui/statusBarMode.test.ts. What cannot be
    // reached from a server render — repoStore state arrives from Tauri — is
    // whether the bar actually hands it a parked operation, the conflict
    // count and the watcher state, so that wiring is pinned here: dropping
    // one would leave a hidden bar silent about it.
    const wiring = source.slice(
      source.indexOf("resolveStatusBarMode($interfaceStore.statusBarMode"),
      source.indexOf("let detail ="),
    );
    expect(wiring).toContain("operationParked: Boolean(operationMarker)");
    expect(wiring).toContain("conflictedCount,");
    expect(wiring).toContain("watchDegraded: Boolean(watchLabel)");
  });
});
