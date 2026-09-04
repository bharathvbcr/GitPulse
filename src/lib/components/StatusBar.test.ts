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
