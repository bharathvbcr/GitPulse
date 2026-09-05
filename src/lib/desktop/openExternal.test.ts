import { readFileSync, readdirSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { createOpener, openExternal } from "./openExternal";

const here = dirname(fileURLToPath(import.meta.url));
const frontendRoot = join(here, "..", "..");

/**
 * Every shipped frontend source. Test files are excluded: they run in Node,
 * never in a webview, so what they import proves nothing about the app.
 */
function shippedSources(dir: string, out: string[] = []): string[] {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name);
    if (entry.isDirectory()) {
      if (entry.name === "node_modules" || entry.name === "__tests__") continue;
      shippedSources(full, out);
    } else if (/\.(ts|js|svelte)$/.test(entry.name) && !/\.(test|spec)\./.test(entry.name)) {
      out.push(full);
    }
  }
  return out;
}

/** Files whose real import statements pull in `@tauri-apps/plugin-opener`. */
function directPluginImporters(): string[] {
  const importer = /import\s*(?:type\s*)?\{[^}]*\}\s*from\s*["']@tauri-apps\/plugin-opener["']/;
  return shippedSources(frontendRoot)
    .filter((file) => importer.test(readFileSync(file, "utf8")))
    .map((file) => relative(frontendRoot, file).split(/[\\/]/).join("/"));
}

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

  /**
   * Derived, not hand-listed. The previous version named three panels, so the
   * four files that later imported the plugin directly — FileViewer,
   * MediaViewer, FileTreePanel and MarkDevViewer — were invisible to it while
   * it reported success. A guard narrower than its own claim is worse than no
   * guard, because it reads like coverage.
   *
   * One owner also means one place the ACL has to grant, which is what makes
   * `src-tauri/tests/acl_contract.rs` able to demand that the granted command
   * set match the used one exactly.
   */
  it("keeps the opener plugin behind exactly one module", () => {
    expect(directPluginImporters()).toEqual(["lib/desktop/openExternal.ts"]);
  });

  /**
   * `window.open` inside a Tauri webview can navigate the app shell itself,
   * and the URLs handed here come from advisory/GitHub payloads.
   */
  it("keeps window.open out of every shipped source", () => {
    const offenders = shippedSources(frontendRoot)
      .filter((file) => readFileSync(file, "utf8").includes("window.open("))
      .map((file) => relative(frontendRoot, file).split(/[\\/]/).join("/"));
    expect(offenders).toEqual([]);
  });

  /** The scan must be able to fail, or it proves nothing. */
  it("scans a real, populated frontend tree", () => {
    expect(shippedSources(frontendRoot).length).toBeGreaterThan(50);
  });
});
