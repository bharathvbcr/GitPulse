import { describe, expect, it } from "vitest";
import {
  gitpulseManualChunk,
  gitpulseTauriFullReload,
  tauriHotUpdateDecision,
} from "../vite.config.ts";

describe("gitpulseManualChunk", () => {
  it("isolates the large runtimes from the application entry", () => {
    expect(gitpulseManualChunk("/repo/node_modules/@xterm/xterm/lib/xterm.js")).toBe(
      "vendor-xterm",
    );
    expect(gitpulseManualChunk("/repo/node_modules/lucide-svelte/dist/icons/bug.svelte")).toBe(
      "vendor-icons",
    );
    expect(gitpulseManualChunk("/repo/node_modules/svelte/src/internal/client/index.js")).toBe(
      "vendor-svelte",
    );
    expect(gitpulseManualChunk("/repo/node_modules/@tauri-apps/api/core.js")).toBe(
      "vendor-tauri",
    );
  });

  it("is cross-platform and leaves application modules in the entry graph", () => {
    expect(gitpulseManualChunk("C:\\repo\\node_modules\\@xterm\\addon-fit\\lib.js")).toBe(
      "vendor-xterm",
    );
    expect(gitpulseManualChunk("/repo/src/lib/components/CoverageViewer.svelte")).toBeUndefined();
    expect(gitpulseManualChunk("/repo/node_modules/tiny-package/index.js")).toBe("vendor");
  });
});

describe("tauriHotUpdateDecision", () => {
  it("leaves browser HMR alone when Tauri env vars are absent", () => {
    expect(tauriHotUpdateDecision({}, "client")).toEqual({
      reloadClient: false,
      modules: undefined,
    });
    expect(tauriHotUpdateDecision({}, "ssr")).toEqual({
      reloadClient: false,
      modules: undefined,
    });
  });

  it("suppresses ESM HMR under Tauri and reloads only the client environment", () => {
    const env = { TAURI_ENV_PLATFORM: "darwin" };
    expect(tauriHotUpdateDecision(env, "client")).toEqual({
      reloadClient: true,
      modules: [],
    });
    expect(tauriHotUpdateDecision(env, "ssr")).toEqual({
      reloadClient: false,
      modules: [],
    });
  });
});

describe("gitpulseTauriFullReload", () => {
  it("registers a pre-order hotUpdate hook so Svelte cannot HMR first", () => {
    const plugin = gitpulseTauriFullReload({ TAURI_ENV_PLATFORM: "darwin" });
    expect(plugin.name).toBe("gitpulse-tauri-full-reload");
    expect(plugin.hotUpdate).toMatchObject({ order: "pre" });
    expect(plugin.handleHotUpdate).toBeUndefined();
  });
});
