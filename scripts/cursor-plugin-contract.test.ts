import { existsSync, readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { appVersion } from "./app-version.mjs";

/**
 * Cursor is the only client of the three that renders a plugin logo.
 *
 * Claude Code has no field for one — not in `plugin.json` (its documented
 * manifest lists none), not in `hooks.json`, not in `.mcp.json` — and the
 * Agent Plugins 1.0.0 schema sets `additionalProperties: false`, so putting
 * `logo` in the portable manifest makes a conformant client reject the
 * package. That asymmetry is the whole reason this file exists: it pins where
 * the logo may live, so a later edit cannot quietly move it somewhere that
 * silently drops it or invalidates the package.
 */
const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const PLUGIN = path.join(ROOT, "plugins", "gitpulse");

function readJson(file: string): Record<string, unknown> {
  return JSON.parse(readFileSync(file, "utf8")) as Record<string, unknown>;
}

describe("native Cursor plugin package", () => {
  const manifestPath = path.join(PLUGIN, ".cursor-plugin", "plugin.json");

  it("carries a manifest whose identity and version match the package", () => {
    expect(existsSync(manifestPath), `${manifestPath} exists`).toBe(true);
    const manifest = readJson(manifestPath);
    expect(manifest.name).toBe(path.basename(PLUGIN));
    // Derived, never typed — see the note in codex-plugin-contract.test.ts.
    // `check-release-version.mjs` lists this manifest in
    // OPTIONAL_PLUGIN_MANIFESTS so it cannot drift from the others.
    expect(manifest.version).toBe(appVersion());
  });

  it("names the app rather than the package slug", () => {
    expect(readJson(manifestPath).displayName).toBe("GitPulse");
  });

  it("points at a logo that is really there", () => {
    // A `logo` naming a file that does not exist is the failure this whole
    // surface rots into: the manifest still parses, the client shows nothing,
    // and no other check looks. Resolve it and read the bytes.
    const logo = readJson(manifestPath).logo;
    expect(typeof logo).toBe("string");
    const rel = String(logo);
    expect(path.isAbsolute(rel), "logo must be package-relative").toBe(false);

    const resolved = path.resolve(PLUGIN, rel);
    expect(
      resolved.startsWith(PLUGIN + path.sep),
      "logo must stay inside the package",
    ).toBe(true);
    expect(existsSync(resolved), `${rel} exists`).toBe(true);

    const bytes = readFileSync(resolved);
    expect(bytes.byteLength).toBeGreaterThan(0);
    // PNG magic. An HTML error page or an LFS pointer saved under a .png name
    // parses as neither, and would otherwise ship as a broken image.
    expect([...bytes.subarray(0, 8)]).toEqual([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
  });
});

describe("logo placement across the per-client manifests", () => {
  it("keeps `logo` out of the portable manifest, which forbids extra fields", () => {
    const portable = readJson(path.join(PLUGIN, "plugin.json"));
    expect(portable.logo).toBeUndefined();
    expect(portable.displayName).toBeUndefined();
  });

  it("gives Claude the display name it renders and no logo field it does not", () => {
    const claude = readJson(path.join(PLUGIN, ".claude-plugin", "plugin.json"));
    expect(claude.displayName).toBe("GitPulse");
    expect(claude.logo).toBeUndefined();
  });
});
