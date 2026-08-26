import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { createOpener, openExternal } from "./openExternal";

const here = dirname(fileURLToPath(import.meta.url));
const panelSource = (name: string) =>
  readFileSync(join(here, "..", "components", name), "utf8");

describe("createOpener", () => {
  it("hands the URL to the injected opener and resolves when it does", async () => {
    const calls: string[] = [];
    const open = createOpener(async (url) => {
      calls.push(url);
    });
    await open("https://github.com/example/repo");
    expect(calls).toEqual(["https://github.com/example/repo"]);
  });

  it("propagates opener failures instead of swallowing them", async () => {
    const open = createOpener(async () => {
      throw new Error("opener permission denied");
    });
    await expect(open("https://github.com/example/repo")).rejects.toThrow(
      "opener permission denied",
    );
  });

  it("fails loud on blank URLs without touching the opener", async () => {
    let calls = 0;
    const open = createOpener(async () => {
      calls += 1;
    });
    await expect(open("   ")).rejects.toThrow("empty URL");
    expect(calls).toBe(0);
  });
});

describe("openExternal canonical adoption", () => {
  it("is bound to the Tauri opener plugin", () => {
    expect(typeof openExternal).toBe("function");
  });

  it("routes all three former copies through the shared module", () => {
    for (const name of ["GitHubPanel.svelte", "HealthPanel.svelte", "ManviOpsPanel.svelte"]) {
      expect(panelSource(name)).toContain('from "../desktop/openExternal"');
    }
  });

  it("keeps window.open out of ManviOpsPanel — no webview-shell navigation fallback", () => {
    const source = panelSource("ManviOpsPanel.svelte");
    expect(source).not.toContain("window.open");
    // The local copy must be gone; only the shared import remains.
    expect(source).not.toContain("@tauri-apps/plugin-opener");
  });

  it("keeps the direct plugin import out of GitHubPanel and HealthPanel too", () => {
    for (const name of ["GitHubPanel.svelte", "HealthPanel.svelte"]) {
      expect(panelSource(name)).not.toContain("@tauri-apps/plugin-opener");
    }
  });
});
