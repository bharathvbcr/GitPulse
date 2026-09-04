import { existsSync, readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const PLUGIN = path.join(ROOT, "plugins", "gitpulse");

function readJson(file: string): Record<string, unknown> {
  return JSON.parse(readFileSync(file, "utf8")) as Record<string, unknown>;
}

describe("native Codex plugin package", () => {
  it("has one canonical package root", () => {
    expect(existsSync(path.join(ROOT, "plugin"))).toBe(false);
  });

  it("uses a package directory whose name matches its native manifest", () => {
    const manifestPath = path.join(PLUGIN, ".codex-plugin", "plugin.json");
    expect(existsSync(manifestPath), `${manifestPath} exists`).toBe(true);
    const manifest = readJson(manifestPath);
    expect(manifest.name).toBe(path.basename(PLUGIN));
    expect(manifest.version).toBe("0.0.5");
    expect(manifest.skills).toBe("./skills/");
    expect(manifest.mcpServers).toBe("./.mcp.json");
  });

  it("declares one portable stdio server and exposes the shared skills", () => {
    const mcp = readJson(path.join(PLUGIN, ".mcp.json"));
    expect(mcp).toEqual({
      mcpServers: {
        gitpulse: {
          type: "stdio",
          command: "gitpulse-mcp",
        },
      },
    });
    expect(existsSync(path.join(PLUGIN, "skills", "gitpulse-insights", "SKILL.md"))).toBe(true);
    expect(existsSync(path.join(PLUGIN, "skills", "gitpulse-collisions", "SKILL.md"))).toBe(true);
  });
});

describe("Codex marketplace and app-bundle discovery", () => {
  it("publishes GitPulse as an installable repo plugin", () => {
    const marketplace = readJson(path.join(ROOT, ".agents", "plugins", "marketplace.json"));
    const plugins = marketplace.plugins as Array<Record<string, unknown>>;
    const entry = plugins.find((candidate) => candidate.name === "gitpulse");
    expect(entry).toMatchObject({
      name: "gitpulse",
      source: { source: "local", path: "./plugins/gitpulse" },
      policy: { installation: "AVAILABLE", authentication: "ON_INSTALL" },
      category: "Developer Tools",
    });
  });

  it("copies that same package into the macOS app resources", () => {
    const tauri = readJson(path.join(ROOT, "src-tauri", "tauri.conf.json"));
    const bundle = tauri.bundle as Record<string, unknown>;
    expect(bundle.resources).toMatchObject({
      "../plugins/gitpulse/": "plugin/",
    });
  });
});
