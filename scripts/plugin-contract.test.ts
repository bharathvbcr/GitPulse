import { readdirSync, readFileSync, statSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

/**
 * Agent Plugins 1.0.0 package at plugin/.
 *
 * Canonical spec: https://agent-plugins.org/specification
 * Schemas: https://agent-plugins.org/schemas/1.0.0/plugin.schema.json
 *          https://agent-plugins.org/schemas/1.0.0/mcp.schema.json
 *
 * Clients load these files; they are not documentation. A typo in `$schema`
 * or an extra top-level field makes a conformant client reject or ignore the
 * package.
 */
const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const PLUGIN = path.join(ROOT, "plugin");
const PLUGIN_SCHEMA = "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json";
const MCP_SCHEMA = "https://agent-plugins.org/schemas/1.0.0/mcp.schema.json";

const MANIFEST_FIELDS = new Set([
  "$schema",
  "name",
  "version",
  "description",
  "author",
  "homepage",
  "repository",
  "license",
  "keywords",
  "extensions",
]);

function loadJson(file: string): Record<string, unknown> {
  return JSON.parse(readFileSync(path.join(PLUGIN, file), "utf8")) as Record<string, unknown>;
}

describe("Agent Plugins 1.0 plugin.json", () => {
  const manifest = loadJson("plugin.json");

  it("declares the 1.0.0 schema and a valid plugin name", () => {
    expect(manifest.$schema).toBe(PLUGIN_SCHEMA);
    expect(manifest.name).toBe("gitpulse");
    const name = String(manifest.name);
    expect(name.length).toBeGreaterThanOrEqual(1);
    expect(name.length).toBeLessThanOrEqual(64);
    expect(name).toMatch(/^[a-z0-9](?:[a-z0-9.-]*[a-z0-9])?$/);
    expect(name).not.toContain("--");
    expect(name).not.toContain("..");
  });

  it("is a closed manifest: no extra top-level fields", () => {
    for (const key of Object.keys(manifest)) {
      expect(MANIFEST_FIELDS, `unknown field ${key}`).toContain(key);
    }
  });
});

describe("Agent Plugins 1.0 mcp.json", () => {
  const mcp = loadJson("mcp.json");
  const plugin = loadJson("plugin.json");

  it("declares the matching 1.0.0 MCP schema and only the two top-level keys", () => {
    expect(mcp.$schema).toBe(MCP_SCHEMA);
    expect(Object.keys(mcp).sort()).toEqual(["$schema", "mcpServers"]);
    const pluginVersion = String(plugin.$schema).match(/\/schemas\/([^/]+)\//)?.[1];
    const mcpVersion = String(mcp.$schema).match(/\/schemas\/([^/]+)\//)?.[1];
    expect(mcpVersion).toBe(pluginVersion);
  });

  it("declares a stdio server whose command is one executable token", () => {
    const servers = mcp.mcpServers as Record<string, Record<string, unknown>>;
    expect(servers.gitpulse).toBeDefined();
    const server = servers.gitpulse;
    expect(server.type).toBe("stdio");
    expect(typeof server.command).toBe("string");
    const command = String(server.command);
    expect(command.length).toBeGreaterThan(0);
    expect(command).not.toContain(" ");
    expect(command === "gitpulse-mcp" || command.startsWith("./")).toBe(true);
  });
});

describe("Agent Plugins 1.0 skills", () => {
  it("discovers only immediate child directories that contain SKILL.md", () => {
    const skillsDir = path.join(PLUGIN, "skills");
    const names = readdirSync(skillsDir).filter((name) =>
      statSync(path.join(skillsDir, name)).isDirectory(),
    );
    expect(names.length).toBeGreaterThan(0);
    for (const name of names) {
      const skill = readFileSync(path.join(skillsDir, name, "SKILL.md"), "utf8");
      const frontmatter = skill.match(/^---\n([\s\S]*?)\n---/)?.[1] ?? "";
      expect(frontmatter, `${name} has YAML frontmatter`).not.toBe("");
      expect(frontmatter).toMatch(/^name:\s+/m);
      expect(frontmatter).toMatch(/^description:\s+/m);
      const skillName = frontmatter.match(/^name:\s+(\S+)/m)?.[1];
      expect(skillName).toBe(name);
    }
  });
});
