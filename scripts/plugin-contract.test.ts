import { execFileSync } from "node:child_process";
import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

/**
 * Agent Plugins 1.0.0 compatibility surface in the canonical GitPulse plugin.
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
const PLUGIN = path.join(ROOT, "plugins", "gitpulse");
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

/**
 * One package, several agent clients. Claude Code discovers it through
 * `.claude-plugin/marketplace.json` at the repo root and then reads
 * `<source>/.claude-plugin/plugin.json` and `<source>/.mcp.json`; the Agent
 * Plugins spec reads `plugin.json` and `mcp.json` in the same directory.
 *
 * Everything below is derived from the marketplace's own `source`, never from
 * a path written here. The package moved once already (`plugin/` ->
 * `plugins/gitpulse/`) and a hardcoded path would have kept asserting against
 * the copy nobody installs.
 *
 * Version agreement is deliberately not asserted here — `check-release-version.mjs`
 * owns that for every manifest in the repo, discovered the same way.
 */
const MARKETPLACE = path.join(ROOT, ".claude-plugin", "marketplace.json");

function loadJsonAt(file: string): Record<string, unknown> {
  return JSON.parse(readFileSync(file, "utf8")) as Record<string, unknown>;
}

const marketplace = loadJsonAt(MARKETPLACE);
const entries = (marketplace.plugins ?? []) as Array<Record<string, unknown>>;

let gitAvailable = true;
try {
  execFileSync("git", ["rev-parse", "--git-dir"], { cwd: ROOT, stdio: "ignore" });
} catch {
  gitAvailable = false;
}

/** True when git would refuse to track `file`. */
function isIgnored(file: string): boolean {
  try {
    // Exit 0 means ignored, 1 means not. `-v` also prints negation matches, so
    // the status is the only trustworthy signal here.
    execFileSync("git", ["check-ignore", "-q", file], { cwd: ROOT, stdio: "ignore" });
    return true;
  } catch {
    return false;
  }
}

describe("Claude Code marketplace manifest", () => {
  it("names an owner and at least one plugin", () => {
    expect(String(marketplace.name)).toMatch(/^[a-z0-9](?:[a-z0-9.-]*[a-z0-9])?$/);
    expect(typeof marketplace.description).toBe("string");
    expect(marketplace.owner).toBeDefined();
    expect(entries.length).toBeGreaterThan(0);
  });

  it("uses repo-relative sources that cannot escape the marketplace root", () => {
    for (const entry of entries) {
      const source = String(entry.source);
      expect(source.startsWith("./"), `${source} must be repo-relative`).toBe(true);
      expect(source).not.toContain("..");
    }
  });

  for (const entry of entries) {
    const source = String(entry.source);
    const dir = path.join(ROOT, source);

    describe(`source ${source}`, () => {
      it("exists and carries both Claude Code files plus skills", () => {
        expect(existsSync(dir), `${source} exists`).toBe(true);
        for (const rel of [".claude-plugin/plugin.json", ".mcp.json"]) {
          expect(existsSync(path.join(dir, rel)), `${source}/${rel}`).toBe(true);
        }
        const skills = readdirSync(path.join(dir, "skills")).filter((name) =>
          statSync(path.join(dir, "skills", name)).isDirectory(),
        );
        expect(skills.length).toBeGreaterThan(0);
      });

      it.runIf(gitAvailable)("keeps every file in the package committable", () => {
        // `.gitignore` carries a broad `.claude*` rule, so a package whose
        // manifest lives in a dot-directory is one un-negated pattern away
        // from being absent on a fresh clone — where it would fail as
        // "missing" rather than "ignored", far from the cause.
        const files: string[] = [];
        const walk = (current: string) => {
          for (const name of readdirSync(current)) {
            const full = path.join(current, name);
            if (statSync(full).isDirectory()) walk(full);
            else files.push(full);
          }
        };
        walk(dir);
        const ignored = files.filter(isIgnored).map((file) => path.relative(ROOT, file));
        expect(ignored, `these would not survive a clone: ${ignored.join(", ")}`).toEqual([]);
      });

      it("mirrors the Agent Plugins manifest field for field, minus $schema", () => {
        const agent = loadJsonAt(path.join(dir, "plugin.json"));
        const claude = loadJsonAt(path.join(dir, ".claude-plugin", "plugin.json"));
        for (const field of ["name", "description", "author", "homepage", "repository", "license", "keywords"]) {
          expect(claude[field], `${field} must match ${source}/plugin.json`).toEqual(agent[field]);
        }
        // Claude Code's manifest has no $schema of its own; carrying the Agent
        // Plugins one over would point a validator at the wrong schema.
        expect(claude.$schema).toBeUndefined();
        expect(typeof claude.version).toBe("string");
      });

      it("declares the identical stdio server to both clients", () => {
        expect(loadJsonAt(path.join(dir, ".mcp.json")).mcpServers).toEqual(
          loadJsonAt(path.join(dir, "mcp.json")).mcpServers,
        );
        expect(Object.keys(loadJsonAt(path.join(dir, ".mcp.json")))).toEqual(["mcpServers"]);
      });

      it("agrees with the marketplace entry that advertises it", () => {
        const claude = loadJsonAt(path.join(dir, ".claude-plugin", "plugin.json"));
        expect(entry.name).toBe(claude.name);
        expect(entry.description).toBe(claude.description);
        expect(entry.license).toBe(claude.license);
        expect(entry.homepage).toBe(claude.homepage);
        expect(entry.keywords).toEqual(claude.keywords);
      });
    });
  }
});
