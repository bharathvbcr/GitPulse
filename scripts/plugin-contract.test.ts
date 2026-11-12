import { execFileSync } from "node:child_process";
import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { declaredBinaryNames } from "./check-release-assets.mjs";
import { bareHookCommand, WORKTREE_LAUNCHER_PREFIX } from "./check-mcp-install.mjs";

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

/**
 * Hooks are the half of the plugin a client executes rather than reads.
 *
 * They live at `<plugin>/hooks/hooks.json` — NOT inside `.claude-plugin/`,
 * which holds only `plugin.json`. A file in the wrong place is not an error a
 * client reports; the hooks are simply never registered, and the package looks
 * installed and does nothing.
 */
describe("plugin hooks", () => {
  const HOOKS = path.join(PLUGIN, "hooks", "hooks.json");

  it("lives at hooks/hooks.json, outside .claude-plugin", () => {
    expect(existsSync(HOOKS)).toBe(true);
    expect(existsSync(path.join(PLUGIN, ".claude-plugin", "hooks.json"))).toBe(false);
  });

  const hooks = JSON.parse(readFileSync(HOOKS, "utf8")) as {
    hooks: Record<string, Array<{ matcher?: string; hooks: Array<Record<string, unknown>> }>>;
  };

  it("declares only hook events this plugin actually handles", () => {
    // An event named here that the binary has no subcommand for spawns a
    // process per matching tool call to do nothing. Spelled out rather than
    // derived so adding an event is a deliberate act: each one costs the user
    // a process. Everything but `PreToolUse` and `SessionStart` reports rather
    // than decides.
    expect(Object.keys(hooks.hooks).sort()).toEqual([
      "Notification",
      "PermissionRequest",
      "PostToolUse",
      "PostToolUseFailure",
      "PreToolUse",
      "SessionEnd",
      "SessionStart",
      "Stop",
      "StopFailure",
      "UserPromptSubmit",
      "WorktreeCreate",
      "WorktreeRemove",
    ]);
  });

  it("runs the worktree hooks, and only them, through the launcher that falls back to plain git", () => {
    // A WorktreeCreate hook that cannot start fails closed: Claude Code gets no
    // worktree. The launcher runs plain git instead when the binary is absent,
    // too old, or hands the job back (exit 75).
    const rust = readFileSync(path.join(ROOT, "src-tauri", "src", "hooks", "worktree.rs"), "utf8");
    const declared = /pub const LAUNCHER_PREFIX: &str = "((?:[^"\\]|\\.)*)";/.exec(rust)?.[1];
    expect(declared, "LAUNCHER_PREFIX is no longer a plain string in hooks/worktree.rs").toBeTruthy();
    expect(JSON.parse(`"${declared}"`)).toBe(WORKTREE_LAUNCHER_PREFIX);
    expect(existsSync(path.join(PLUGIN, "hooks", "gitpulse-worktree.sh"))).toBe(true);
    for (const [event, groups] of Object.entries(hooks.hooks)) {
      for (const group of groups) {
        for (const handler of group.hooks) {
          const launched = String(handler.command).startsWith(WORKTREE_LAUNCHER_PREFIX);
          expect(launched, `${event}: ${handler.command}`).toBe(event === "WorktreeCreate" || event === "WorktreeRemove");
        }
      }
    }
    expect(bareHookCommand(String(hooks.hooks.WorktreeCreate[0].hooks[0].command))).toBe("gitpulse-hook worktree-create");
    expect(bareHookCommand(String(hooks.hooks.WorktreeRemove[0].hooks[0].command))).toBe("gitpulse-hook worktree-remove");
  });

  it("never makes the agent wait on a report it could have sent in the background", () => {
    // `PostToolUse` runs after every tool call and `UserPromptSubmit` before
    // every prompt; a report holding either would tax every turn for a side
    // effect. Async hooks cannot decide anything — which a report never does.
    // `Notification` is already off the agent's path, and `SessionEnd` runs at
    // teardown, where a background hook may be killed before it reports.
    const mustBeAsync = new Set(["PermissionRequest", "PostToolUse", "PostToolUseFailure", "Stop", "UserPromptSubmit"]);
    for (const [event, groups] of Object.entries(hooks.hooks)) {
      for (const group of groups) {
        for (const handler of group.hooks) {
          const subcommand = bareHookCommand(String(handler.command)).split(/\s+/)[1];
          const reports = subcommand === "notify";
          const injectsPeerInbox = subcommand === "peer-inbox";
          // `deliver` is the one hook that waits, in the background: `asyncRewake`
          // runs it without blocking the agent and wakes the agent on exit 2. It
          // is not `async`, which "cannot block" and so cannot wake.
          const waits = subcommand === "deliver";
          if (waits) {
            expect(event, "deliver only makes sense where a turn ends").toBe("Stop");
            expect(handler.asyncRewake, "deliver is asyncRewake").toBe(true);
            expect(handler.async, "an async hook cannot wake the agent").toBeUndefined();
            continue;
          }
          if (injectsPeerInbox) {
            expect(["PostToolUse", "SessionStart", "UserPromptSubmit"]).toContain(event);
            expect(handler.async, "peer-inbox injects context synchronously").toBeUndefined();
            expect(handler.asyncRewake, "peer-inbox is not a background waiter").toBeUndefined();
            continue;
          }
          if (mustBeAsync.has(event)) expect(handler.async, `${event} blocks the agent on a report`).toBe(true);
          // A deciding hook that ran in the background would decide nothing.
          if (!reports) expect(handler.async, `${event} decides in the background`).toBeUndefined();
          expect(handler.asyncRewake, `${event} wakes the agent from something that is not deliver`).toBeUndefined();
        }
      }
    }
    // A matcher on these would silently skip calls; each must see every one.
    for (const event of ["PermissionRequest", "PostToolUse", "PostToolUseFailure"]) {
      for (const group of hooks.hooks[event]) expect(group.matcher, event).toBeUndefined();
    }
  });

  it("spawns a bounded command for every entry", () => {
    for (const [event, groups] of Object.entries(hooks.hooks)) {
      for (const group of groups) {
        expect(Array.isArray(group.hooks), `${event} group has no hooks`).toBe(true);
        for (const handler of group.hooks) {
          expect(handler.type, `${event}`).toBe("command");
          expect(typeof handler.command, `${event}`).toBe("string");
          // A hook runs on every matching tool call. One without a timeout can
          // hang the user's editor indefinitely.
          expect(typeof handler.timeout, `${event} has no timeout`).toBe("number");
          expect(handler.timeout as number).toBeGreaterThan(0);
          // An `asyncRewake` waiter is the one entry that lives for minutes: it
          // runs in the background, so no tool call waits on it, and Claude Code
          // enforces its `timeout` (default 600 s) by killing it. The worktree
          // hooks run once per worktree, not per tool call, and Claude Code
          // waits for the worktree it asked for: they keep that 600 s default,
          // written down. Everything else is on the agent's path and stays
          // under a minute.
          const worktree = event === "WorktreeCreate" || event === "WorktreeRemove";
          const ceiling = handler.asyncRewake === true ? 1800 : worktree ? 600 : 60;
          expect(handler.timeout as number, `${event} ${handler.command}`).toBeLessThanOrEqual(ceiling);
        }
      }
    }
  });

  it("invokes the same binary this repo builds, with a real subcommand", () => {
    // The MCP manifest resolves `gitpulse-mcp` off PATH for the same reason a
    // published plugin must: the client runs the installed server, not a path
    // baked into a manifest. The hook binary follows that, so the subcommand is
    // the part a typo can break silently.
    const declared = new Set<string>();
    for (const groups of Object.values(hooks.hooks)) {
      for (const group of groups) {
        for (const handler of group.hooks) {
          const [bin, ...rest] = bareHookCommand(String(handler.command)).split(/\s+/);
          expect(bin).toBe("gitpulse-hook");
          expect(rest.length, `${handler.command} names no subcommand`).toBeGreaterThan(0);
          // Only `notify` takes a second word, and it is the notification type
          // the matcher routed. A stray argument anywhere else is a typo the
          // binary would silently ignore.
          expect(rest.length, `${handler.command} passes more than one argument`).toBeLessThanOrEqual(2);
          if (rest.length === 2) {
            expect(rest[0], `${handler.command} argues with a subcommand that takes none`).toBe("notify");
          }
          declared.add(rest[0]);
        }
      }
    }
    // Derived from the Rust source rather than listed here, so a subcommand
    // renamed on one side fails instead of silently going unhandled.
    const source = readFileSync(
      path.join(ROOT, "src-tauri", "src", "hooks", "mod.rs"),
      "utf8",
    );
    const known = source.match(/pub const SUBCOMMANDS: \[&str; \d+\] = \[([^\]]*)\]/)?.[1] ?? "";
    const implemented = new Set(
      [...known.matchAll(/"([a-z-]+)"/g)].map((m) => m[1]),
    );
    expect(implemented.size).toBeGreaterThan(0);
    for (const subcommand of declared) {
      expect(implemented, `${subcommand} is declared but not implemented`).toContain(subcommand);
    }
  });

  it("matches tool names with anchored patterns", () => {
    // `Edit` unanchored also matches `NotebookEdit` and any MCP tool whose name
    // contains it, so a guard meant for three tools fires on many.
    for (const group of hooks.hooks.PreToolUse) {
      const matcher = String(group.matcher);
      expect(matcher.startsWith("^") && matcher.endsWith("$"), matcher).toBe(true);
    }
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

/**
 * Every executable the package spawns must be one `bun run mcp:install` puts
 * on PATH.
 *
 * This is the half of the contract that lives outside the package. The files
 * above prove the manifests are internally consistent; none of them can see
 * whether the binaries those manifests name will exist on the machine that
 * installs them. They did not: `mcp:install` built only `gitpulse-mcp` while
 * `hooks/hooks.json` spawned `gitpulse-hook`, so a correct, validated,
 * fully-tested plugin shipped with its collision guard and command gate
 * permanently disabled.
 *
 * The two failures are not symmetric, which is why this is worth a test rather
 * than a line in a README. A missing MCP server shows up in the client as a
 * server that would not connect. A missing hook binary does not: the Claude
 * Code hook reference is explicit that a hook which cannot start is a
 * non-blocking error and the tool call proceeds — "a mistyped path in
 * settings.json leaves the gate silently disabled". An uninstallable server is
 * loud; an uninstallable hook is indistinguishable from a hook that ran and
 * found nothing.
 *
 * Both sides are derived. The executables come from the manifests a client
 * reads, and the installed list from the script a developer runs, so adding a
 * hook or renaming a binary fails here instead of shipping.
 */
describe("the package's executables are installable", () => {
  /** Every bare executable token the shipped manifests ask a host to spawn. */
  function spawnedExecutables(): Set<string> {
    const found = new Set<string>();

    const hooks = JSON.parse(
      readFileSync(path.join(PLUGIN, "hooks", "hooks.json"), "utf8"),
    ) as { hooks: Record<string, Array<{ hooks: Array<{ command?: string }> }>> };
    for (const groups of Object.values(hooks.hooks)) {
      for (const group of groups) {
        for (const handler of group.hooks) {
          // The worktree launcher is a script in the package, run by the
          // host's `sh`; what it spawns is the binary named after it.
          const bin = bareHookCommand(String(handler.command ?? "")).split(/\s+/)[0];
          if (bin) found.add(bin);
        }
      }
    }

    for (const manifest of [".mcp.json", "mcp.json"]) {
      const servers = (loadJson(manifest).mcpServers ?? {}) as Record<string, { command?: string }>;
      for (const server of Object.values(servers)) {
        if (typeof server.command === "string" && server.command) found.add(server.command);
      }
    }

    const agyHooks = JSON.parse(
      readFileSync(path.join(ROOT, "plugins", "gitpulse-agy", "hooks.json"), "utf8"),
    ) as Record<string, Record<string, Array<{ command?: string; hooks?: Array<{ command?: string }> }>>>;
    for (const namedHook of Object.values(agyHooks)) {
      for (const entries of Object.values(namedHook)) {
        for (const item of entries) {
          const handlers = Array.isArray(item.hooks) ? item.hooks : [item];
          for (const handler of handlers) {
            const bin = bareHookCommand(String(handler.command ?? "")).split(/\s+/)[0];
            if (bin) found.add(bin);
          }
        }
      }
    }
    const agyMcp = JSON.parse(
      readFileSync(path.join(ROOT, "plugins", "gitpulse-agy", "mcp_config.json"), "utf8"),
    ) as { mcpServers?: Record<string, { command?: string }> };
    for (const server of Object.values(agyMcp.mcpServers ?? {})) {
      if (typeof server.command === "string" && server.command) found.add(server.command);
    }
    return found;
  }

  /** The `--bin` names `bun run mcp:install` actually installs. */
  function installedExecutables(): Set<string> {
    const pkg = JSON.parse(readFileSync(path.join(ROOT, "package.json"), "utf8")) as {
      scripts: Record<string, string>;
    };
    const script = pkg.scripts["mcp:install"];
    expect(script, "package.json has no mcp:install script").toBeTruthy();
    return new Set([...script.matchAll(/--bin\s+(\S+)/g)].map((match) => match[1]));
  }

  it("finds executables on both sides, so the comparison is not vacuous", () => {
    expect(spawnedExecutables().size).toBeGreaterThan(0);
    expect(installedExecutables().size).toBeGreaterThan(0);
  });

  it("installs every executable the manifests spawn", () => {
    const installed = installedExecutables();
    for (const bin of spawnedExecutables()) {
      expect(
        installed,
        `the package spawns ${bin} but bun run mcp:install does not install it; a host would get "command not found"`,
      ).toContain(bin);
    }
  });

  it("only installs binaries this crate actually builds", () => {
    // A `--bin` naming a target that does not exist fails the install outright,
    // which would leave the other binary uninstalled too.
    const cargo = readFileSync(path.join(ROOT, "src-tauri", "Cargo.toml"), "utf8");
    const targets = new Set(declaredBinaryNames(cargo));
    expect(targets.size).toBeGreaterThan(0);
    for (const bin of installedExecutables()) {
      expect(targets, `mcp:install names ${bin}, which is not a [[bin]] in Cargo.toml`).toContain(bin);
    }
  });

  it("pins the five [[bin]] names the release bundles must hold, and every executable the manifests spawn is one", () => {
    // release.yml's verify step requires exactly these names inside each
    // installer (check-release-assets.mjs reads them from this same manifest),
    // so a binary a plugin spawns that is not a [[bin]] would never ship, and a
    // new [[bin]] becomes required in every bundle the moment it is declared.
    const cargo = readFileSync(path.join(ROOT, "src-tauri", "Cargo.toml"), "utf8");
    const bins = declaredBinaryNames(cargo);
    expect([...bins].sort()).toEqual(["gitpulse", "gitpulse-hook", "gitpulse-mcp", "gitpulse-ptyd", "gitpulsed"]);
    for (const bin of spawnedExecutables()) {
      expect(bins, `a manifest spawns ${bin}, which no [[bin]] builds`).toContain(bin);
    }
  });

  it("is the install command the docs tell people to run", () => {
    // A second, undocumented install path is how the first one goes stale.
    const contributing = readFileSync(path.join(ROOT, "CONTRIBUTING.md"), "utf8");
    expect(contributing).toContain("bun run mcp:install");
  });
});

/**
 * The hook must give up before the host does.
 *
 * `hooks::BUDGET` is how long one hook invocation waits for the work behind it
 * — a worktree sweep, a harness verdict — before abandoning it. `timeout` in
 * `hooks.json` is how long the host waits for the whole process. The order
 * between them is load-bearing and invisible:
 *
 * - budget first, and a check that could not finish still prints its
 *   "this ran UNGATED" notice, which is the entire honesty contract of the
 *   module. Measured against a deliberately wedged `manvi`: the hook answers
 *   at 5.1s and the user is told.
 * - host first, and the Claude Code hook reference says the output is
 *   discarded and the tool call proceeds. The notice is never written, so a
 *   check that did not run becomes indistinguishable from one that ran and
 *   found nothing — the exact substitution `src/hooks/mod.rs` exists to
 *   prevent, defeated by a number in a JSON file nobody would think to read
 *   alongside it.
 *
 * Nothing else relates these two constants, and they live in different
 * languages in different files, so this is the only place the ordering can be
 * stated. Both sides are parsed rather than repeated.
 */
const hookSource = readFileSync(
  path.join(ROOT, "src-tauri", "src", "hooks", "mod.rs"),
  "utf8",
);
const notifyInputSource = readFileSync(
  path.join(ROOT, "src-tauri", "src", "hooks", "notify_input.rs"),
  "utf8",
);

/** A `const NAME: Duration = Duration::from_secs|millis(N)` in hooks/mod.rs, in seconds. */
function hookDurationSeconds(constant: string): number {
  const declared = new RegExp(
    `const ${constant}: Duration = Duration::from_(secs|millis)\\((\\d+)\\)`,
  ).exec(hookSource);
  expect(declared, `${constant} is not a plain Duration constant; update this contract`).toBeTruthy();
  const value = Number(declared![2]);
  return declared![1] === "secs" ? value : value / 1000;
}

function hookBudgetSeconds(): number {
  const match = /pub const BUDGET: Duration = Duration::from_secs\((\d+)\)/.exec(hookSource);
  expect(match, "hooks::BUDGET is no longer a whole number of seconds; update this contract").toBeTruthy();
  return Number(match![1]);
}

/**
 * The worktree hooks' whole-process deadline: `WORKTREE_BUDGET` in
 * hooks/worktree.rs. Not `TOTAL_BUDGET`: creating a worktree runs git, a cache
 * copy and the repository's approved post_create hooks, and Claude Code waits.
 */
function worktreeBudgetSeconds(): number {
  const source = readFileSync(path.join(ROOT, "src-tauri", "src", "hooks", "worktree.rs"), "utf8");
  const declared = /pub const WORKTREE_BUDGET: Duration = Duration::from_secs\((\d+)\)/.exec(source);
  expect(declared, "WORKTREE_BUDGET is no longer a whole number of seconds; update this contract").toBeTruthy();
  return Number(declared![1]);
}

/**
 * How long `deliver` waits for a message: `DELIVER_WAIT` in hooks/deliver.rs,
 * a plain whole number of seconds. It is not `BUDGET` or `TOTAL_BUDGET`: those
 * are for hooks the agent is waiting on, and a waiter cut to them would give
 * up on an idle session. The entry's `timeout` has to outlast it, or the host
 * kills the waiter mid-wait and a message claimed in that instant is lost.
 */
function deliverWaitSeconds(): number {
  const source = readFileSync(path.join(ROOT, "src-tauri", "src", "hooks", "deliver.rs"), "utf8");
  const declared = /pub const DELIVER_WAIT: Duration = Duration::from_secs\((\d+)\)/.exec(source);
  expect(declared, "DELIVER_WAIT is no longer a whole number of seconds; update this contract").toBeTruthy();
  return Number(declared![1]);
}

/**
 * What one `notify` run waits out for `event`: the read and then the socket
 * report, each for the budget `notify_input::budgets_for` gives that event.
 * Derived from the source, never listed here: the function is read, the two
 * constants it names (one for `session_ended`, one for everything else) are
 * resolved to their plain `Duration`s, and the handlers are checked to take
 * their budgets from it, so a handler that stopped doing so fails this
 * rather than being measured against a number it no longer uses.
 */
function notifyRunSeconds(event: string): number {
  expect(
    /fn run_notify\b[\s\S]*?budgets_for\(/.exec(hookSource),
    "run_notify no longer takes its send budget from budgets_for; update this contract",
  ).toBeTruthy();
  expect(
    /pub fn read_for\b[\s\S]*?budgets_for\(/.exec(hookSource),
    "read_for no longer takes notify's read budget from budgets_for; update this contract",
  ).toBeTruthy();
  const classes = /fn budgets_for\b[\s\S]*?"session_ended"\s*=>\s*([A-Z_]+),\s*_\s*=>\s*([A-Z_]+),/.exec(
    notifyInputSource,
  );
  expect(classes, "budgets_for no longer maps session_ended and the rest to two constants; update this contract").toBeTruthy();
  const each = hookDurationSeconds(event === "session_ended" ? classes![1] : classes![2]);
  return each * 2;
}

/**
 * What one `stop-or-notify` run waits out: `read_for`'s `PEER_INBOX_BUDGET`
 * stdin walk plus `run_stop_or_notify`'s explicit `turn_finished` socket budget.
 */
function stopOrNotifySeconds(): number {
  const readArm =
    /if matches!\(subcommand,\s*"peer-inbox"\s*\|\s*"stop-or-notify"\)[\s\S]*?read_notify_input_within\([\s\S]*?,\s*([A-Z_]+),?\s*\)/.exec(
      hookSource,
    );
  expect(
    readArm,
    "read_for no longer bounds stop-or-notify with a plain Duration constant; update this contract",
  ).toBeTruthy();
  const sendCall =
    /pub fn run_stop_or_notify\b[\s\S]*?run_notify_within\([\s\S]*?Some\(Duration::from_(secs|millis)\((\d+)\)\)/.exec(
      hookSource,
    );
  expect(
    sendCall,
    "run_stop_or_notify no longer passes a plain Duration to run_notify_within; update this contract",
  ).toBeTruthy();
  const sendSeconds =
    sendCall![1] === "secs" ? Number(sendCall![2]) : Number(sendCall![2]) / 1000;
  return hookDurationSeconds(readArm![1]) + sendSeconds;
}

/**
 * The ceiling the work behind one subcommand actually waits out.
 *
 * Not every subcommand shares `BUDGET`. `notify` reports rather than checks,
 * runs while the user is already waiting, and has nothing worth waiting for,
 * so it has its own much shorter one. Which budget applies is *derived* from
 * the source — the subcommand's handler is read and the constant it passes
 * to `within_budget` is the answer — rather than listed here, because a list
 * would go stale exactly when a handler changed its mind.
 */
function budgetSecondsFor(subcommand: string, argument = ""): number {
  if (subcommand === "notify") return notifyRunSeconds(argument);
  if (subcommand === "deliver") return deliverWaitSeconds();
  if (subcommand === "worktree-create" || subcommand === "worktree-remove") return worktreeBudgetSeconds();
  if (subcommand === "stop-or-notify") return stopOrNotifySeconds();
  const fnBlock = new RegExp(
    `fn run_${subcommand.replaceAll("-", "_")}\\b[\\s\\S]*?\\n\\}`,
  ).exec(hookSource)?.[0];
  const handler = fnBlock ? /within_budget\(\s*([A-Z_]+)/.exec(fnBlock) : null;
  if (!handler) return hookBudgetSeconds();
  const constant = handler[1];
  if (constant === "BUDGET") return hookBudgetSeconds();
  return hookDurationSeconds(constant);
}

/**
 * Minimum host `timeout` (in seconds) required for `subcommand` (`argument`) so
 * the internal deadline finishes with `HOST_TIMEOUT_MARGIN` to print stdout and
 * reap children before the host kills the subprocess.
 */
function minHostTimeoutSeconds(subcommand: string, argument = ""): number {
  const margin = hookDurationSeconds("HOST_TIMEOUT_MARGIN");
  if (subcommand === "notify") return notifyRunSeconds(argument) + margin;
  if (subcommand === "worktree-create" || subcommand === "worktree-remove") {
    return worktreeBudgetSeconds() + margin;
  }
  if (subcommand === "peer-inbox") {
    return hookDurationSeconds("PEER_INBOX_BUDGET") + margin;
  }
  if (subcommand === "stop-or-notify") {
    return stopOrNotifySeconds() + margin;
  }
  return hookDurationSeconds("TOTAL_BUDGET") + margin;
}

describe("hook budgets sit inside the host's timeouts", () => {
  function declaredTimeouts(): Array<{
    manifest: "gitpulse" | "gitpulse-agy";
    event: string;
    command: string;
    timeout: number;
  }> {
    const hooks = JSON.parse(
      readFileSync(path.join(PLUGIN, "hooks", "hooks.json"), "utf8"),
    ) as { hooks: Record<string, Array<{ hooks: Array<{ command?: string; timeout?: number }> }>> };
    const rows: Array<{
      manifest: "gitpulse" | "gitpulse-agy";
      event: string;
      command: string;
      timeout: number;
    }> = [];
    for (const [event, groups] of Object.entries(hooks.hooks)) {
      for (const group of groups) {
        for (const handler of group.hooks) {
          rows.push({
            manifest: "gitpulse",
            event,
            command: bareHookCommand(String(handler.command)),
            timeout: Number(handler.timeout),
          });
        }
      }
    }
    const agyHooks = JSON.parse(
      readFileSync(path.join(ROOT, "plugins", "gitpulse-agy", "hooks.json"), "utf8"),
    ) as Record<
      string,
      Record<
        string,
        Array<{ command?: string; timeout?: number; hooks?: Array<{ command?: string; timeout?: number }> }>
      >
    >;
    for (const namedHook of Object.values(agyHooks)) {
      for (const [event, entries] of Object.entries(namedHook)) {
        for (const item of entries) {
          const handlers = Array.isArray(item.hooks) ? item.hooks : [item];
          for (const handler of handlers) {
            rows.push({
              manifest: "gitpulse-agy",
              event,
              command: bareHookCommand(String(handler.command)),
              timeout: Number(handler.timeout),
            });
          }
        }
      }
    }
    return rows;
  }

  it("reads a budget and at least one declared timeout", () => {
    expect(hookBudgetSeconds()).toBeGreaterThan(0);
    expect(declaredTimeouts().filter((r) => r.manifest === "gitpulse").length).toBeGreaterThan(0);
    expect(declaredTimeouts().filter((r) => r.manifest === "gitpulse-agy").length).toBeGreaterThan(0);
  });

  it("gives every hook longer than the work inside it is allowed to take", () => {
    for (const { manifest, event, command, timeout } of declaredTimeouts()) {
      const [, subcommand = "", argument = ""] = command.split(/\s+/);
      const budget = budgetSecondsFor(subcommand, argument);
      expect(Number.isFinite(timeout), `${manifest}:${event} ${command} has no numeric timeout`).toBe(true);
      expect(
        timeout,
        `${manifest}:${event} "${command}" is killed by the host after ${timeout}s, but the hook waits ${budget}s before giving up — the host discards the output of a timed-out hook, so whatever it had to say would never be delivered`,
      ).toBeGreaterThan(budget);
    }
  });

  it("lets the mailbox waiter finish and exit on its own well before the host's timeout kills it", () => {
    const rows = declaredTimeouts().filter((row) => row.command.split(/\s+/)[1] === "deliver");
    expect(rows.length, "the manifest has no deliver entry").toBe(1);
    const wait = deliverWaitSeconds();
    // A margin big enough for a poll, a last claim and a write; the host's
    // timeout is the weaker backstop, never the way a normal wait ends.
    expect(rows[0].timeout - wait).toBeGreaterThanOrEqual(30);
    expect(rows[0].timeout).toBeLessThanOrEqual(1800);
    // And it is not clamped to the short budget the deciding hooks share.
    expect(wait).toBeGreaterThan(hookDurationSeconds("TOTAL_BUDGET"));
  });

  it("ends the whole process, not just each wait, a stated margin before the host's timeout", () => {
    // Each wait is cut to what is left of TOTAL_BUDGET from process start, so
    // stdin + scope check + scan cannot add up to the host's timeout. The margin
    // is what printing the answer and reaping a child need after the work ends.
    const total = hookDurationSeconds("TOTAL_BUDGET");
    const margin = hookDurationSeconds("HOST_TIMEOUT_MARGIN");
    expect(total).toBeGreaterThanOrEqual(hookBudgetSeconds());
    expect(margin).toBeGreaterThan(0);
    for (const { manifest, event, command, timeout } of declaredTimeouts()) {
      const [, subcommand = "", argument = ""] = command.split(/\s+/);
      if (subcommand === "deliver") continue;
      // SessionEnd has its own tighter 1 s exit budget checked below.
      if (event === "SessionEnd") continue;
      const required = minHostTimeoutSeconds(subcommand, argument);
      expect(
        timeout,
        `${manifest}:${event} "${command}" times out at ${timeout}s; its internal budget + ${margin}s margin requires ${required}s`,
      ).toBeGreaterThanOrEqual(required);
    }
  });

  it("reads a different budget for a subcommand that declares one", () => {
    // Guards the derivation itself: if the regex stopped matching, every
    // subcommand would silently fall back to BUDGET and this contract would
    // check the wrong number while still passing.
    expect(budgetSecondsFor("notify", "tool_finished")).toBeLessThan(hookBudgetSeconds());
    // SessionEnd's report has its own, shorter one: the injected copy is held to 1 s.
    expect(budgetSecondsFor("notify", "session_ended")).toBeLessThan(budgetSecondsFor("notify", "tool_finished"));
    expect(budgetSecondsFor("notify", "session_ended")).toBeLessThan(1);
    expect(budgetSecondsFor("peer-inbox")).toBeLessThan(hookBudgetSeconds());
    expect(budgetSecondsFor("stop-or-notify")).toBeLessThan(hookBudgetSeconds());
    expect(budgetSecondsFor("collision-guard")).toBe(hookBudgetSeconds());
  });

  it("never lets SessionEnd's timeout raise Claude Code's 1.5 s exit budget", () => {
    // https://code.claude.com/docs/en/hooks, the `timeout` row: "`SessionEnd`
    // hooks share a 1.5-second budget; if your settings set a longer per-hook
    // `timeout`, Claude Code raises the budget to match, up to 60 seconds".
    // So a longer one holds every exit of the session open.
    const DEFAULT_SESSION_END_BUDGET_S = 1.5;
    const declared = /pub const SESSION_END_HOST_TIMEOUT: Duration = Duration::from_secs\((\d+)\)/.exec(notifyInputSource);
    expect(declared, "SESSION_END_HOST_TIMEOUT is no longer whole seconds; update this contract").toBeTruthy();
    const hostTimeout = Number(declared![1]);
    expect(hostTimeout).toBeLessThanOrEqual(DEFAULT_SESSION_END_BUDGET_S);
    const marginMs = /const EXIT_MARGIN: Duration = Duration::from_millis\((\d+)\)/.exec(
      readFileSync(path.join(ROOT, "src-tauri", "src", "hooks", "notify_input_tests.rs"), "utf8"),
    );
    expect(marginMs, "EXIT_MARGIN is no longer plain milliseconds; update this contract").toBeTruthy();
    expect(Number(marginMs![1])).toBeGreaterThanOrEqual(250);
    const rows = declaredTimeouts().filter((row) => row.event === "SessionEnd");
    expect(rows.length).toBeGreaterThan(0);
    for (const { command, timeout } of rows) {
      expect(timeout, `SessionEnd "${command}" raises the exit budget`).toBeLessThanOrEqual(DEFAULT_SESSION_END_BUDGET_S);
      expect(timeout, `SessionEnd "${command}" differs from the injected copy's timeout`).toBe(hostTimeout);
      const [, subcommand = "", argument = ""] = command.split(/\s+/);
      if (subcommand === "notify") {
        expect(
          budgetSecondsFor(subcommand, argument) + Number(marginMs![1]) / 1000,
          `SessionEnd "${command}" runs past its ${timeout}s less the exit margin`,
        ).toBeLessThanOrEqual(timeout);
      }
    }
  });
});

/**
 * Which host hook reports which event. `fixtures/notify-routes.json` is the
 * table; `hooks::notify_routes_tests` in Rust holds the manifest and the
 * injected settings to the same one. The manifest must carry exactly those
 * routes and the routed names must equal `alerts::bridge::EVENTS` both ways,
 * so deleting or retargeting any one entry fails here, which a count of
 * entries could not do. Mutations run on copies, never the shipped file.
 */
describe("notify routes", () => {
  type Hooks = Record<string, Array<{ matcher?: string; hooks: Array<{ command?: string; timeout?: number }> }>>;
  const fixture = JSON.parse(readFileSync(path.join(ROOT, "scripts", "fixtures", "notify-routes.json"), "utf8")) as {
    routes: Array<{ hook: string; matcher: string | null; event: string; injected: boolean }>;
  };
  const shipped = (JSON.parse(readFileSync(path.join(PLUGIN, "hooks", "hooks.json"), "utf8")) as { hooks: Hooks }).hooks;
  const bridge = readFileSync(path.join(ROOT, "src-tauri", "src", "alerts", "bridge.rs"), "utf8");
  const table = bridge.slice(bridge.indexOf("pub const EVENTS"), bridge.indexOf("];", bridge.indexOf("pub const EVENTS")));
  const EVENTS = [...table.matchAll(/name: "([a-z_]+)"/g)].map((match) => match[1]);

  const key = (hook: string, matcher: string | null | undefined, event: string) => `${hook}|${matcher ?? ""}|${event}`;
  const wanted = fixture.routes.map((row) => key(row.hook, row.matcher, row.event)).sort();

  function routesOf(hooks: Hooks): string[] {
    const found: string[] = [];
    for (const [hook, groups] of Object.entries(hooks)) {
      for (const group of groups) {
        for (const handler of group.hooks) {
          const [, event] = String(handler.command).split(" notify ");
          if (event !== undefined) found.push(key(hook, group.matcher, event.trim()));
        }
      }
    }
    return found.sort();
  }

  function problems(hooks: Hooks): string[] {
    const have = routesOf(hooks);
    const out: string[] = [];
    for (const route of wanted) if (!have.includes(route)) out.push(`missing ${route}`);
    for (const route of have) if (!wanted.includes(route)) out.push(`unexpected ${route}`);
    if (have.length !== wanted.length && out.length === 0) out.push("a route is registered twice");
    const routed = new Set(have.map((route) => route.split("|")[2]));
    for (const name of EVENTS) if (!routed.has(name)) out.push(`EVENTS has ${name}, which nothing routes`);
    for (const name of routed) if (!EVENTS.includes(name)) out.push(`a route reports ${name}, which EVENTS lacks`);
    for (const route of have) {
      const [hook, matcher, event] = route.split("|");
      if (hook === "Notification" && matcher !== event) out.push(`Notification matcher ${matcher} reports ${event}`);
    }
    return out;
  }

  /** Every notify handler as [hook, group index, handler index]. */
  function positions(hooks: Hooks): Array<[string, number, number]> {
    const at: Array<[string, number, number]> = [];
    for (const [hook, groups] of Object.entries(hooks)) {
      groups.forEach((group, g) => group.hooks.forEach((handler, h) => {
        if (String(handler.command).includes(" notify ")) at.push([hook, g, h]);
      }));
    }
    return at;
  }

  it("reads the events and the routes it checks against", () => {
    expect(EVENTS.length).toBeGreaterThanOrEqual(12);
    expect(fixture.routes.length).toBe(13);
  });

  it("routes exactly the events the socket serves, through exactly the table's entries", () => {
    expect(problems(shipped)).toEqual([]);
  });

  it("injects exactly the table's flagged hooks (plus PreToolUse's guards)", () => {
    const claudeHook = readFileSync(path.join(ROOT, "src-tauri", "src", "workbench", "claude_hook.rs"), "utf8");
    const list = /const INJECTED_EVENTS: \[&str; \d+\] = \[([^\]]*)\]/.exec(claudeHook)?.[1] ?? "";
    const injected = [...list.matchAll(/"([A-Za-z]+)"/g)].map((match) => match[1]);
    const flagged = [...new Set(fixture.routes.filter((row) => row.injected).map((row) => row.hook))];
    expect([...flagged, "PreToolUse"].sort()).toEqual([...injected].sort());
  });

  it("fails when any notify entry is deleted", () => {
    const at = positions(shipped);
    expect(at.length).toBe(13);
    for (const [hook, g, h] of at) {
      const copy = structuredClone(shipped);
      copy[hook][g].hooks.splice(h, 1);
      expect(problems(copy), `deleting ${hook}[${g}][${h}] went unnoticed`).not.toEqual([]);
    }
  });

  it("fails when any notify entry is retargeted to another event", () => {
    for (const [hook, g, h] of positions(shipped)) {
      for (const event of EVENTS) {
        const copy = structuredClone(shipped);
        const head = String(copy[hook][g].hooks[h].command).split(" notify ")[0];
        copy[hook][g].hooks[h].command = `${head} notify ${event}`;
        if (routesOf(copy).join() === routesOf(shipped).join()) continue;
        expect(problems(copy), `${hook}[${g}][${h}] retargeted to ${event} went unnoticed`).not.toEqual([]);
      }
    }
    // The retarget a count cannot see: SessionEnd reporting turn_finished.
    const copy = structuredClone(shipped);
    copy.SessionEnd[0].hooks[0].command = "gitpulse-hook notify turn_finished";
    expect(problems(copy)).toContain("EVENTS has session_ended, which nothing routes");
  });
});

describe("Antigravity plugin (plugins/gitpulse-agy)", () => {
  const AGY_PLUGIN = path.join(ROOT, "plugins", "gitpulse-agy");

  type AgyHandler = {
    type?: unknown;
    command?: unknown;
    timeout?: unknown;
    env?: Record<string, unknown>;
  };
  type AgyEntry = AgyHandler & {
    matcher?: unknown;
    hooks?: AgyHandler[];
  };

  const hooks = loadJsonAt(path.join(AGY_PLUGIN, "hooks.json")) as Record<
    string,
    Record<string, AgyEntry[]>
  >;
  const known = hookSource.match(/pub const SUBCOMMANDS: \[&str; \d+\] = \[([^\]]*)\]/)?.[1] ?? "";
  const implemented = new Set([...known.matchAll(/"([a-z-]+)"/g)].map((m) => m[1]));

  const bridge = readFileSync(path.join(ROOT, "src-tauri", "src", "alerts", "bridge.rs"), "utf8");
  const bridgeTable = bridge.slice(
    bridge.indexOf("pub const EVENTS"),
    bridge.indexOf("];", bridge.indexOf("pub const EVENTS")),
  );
  const notifyEvents = new Set([...bridgeTable.matchAll(/name: "([a-z_]+)"/g)].map((m) => m[1]));

  const incomingFn = /fn incoming_edit\([\s\S]*?\n\}\n/.exec(hookSource)?.[0] ?? "";
  const armHeaders = [...incomingFn.matchAll(/^\s{8}("[^"]+"(?:\s*\|\s*"[^"]+")*)\s*=>/gm)].map(
    (m) => m[1],
  );
  const agyEditTools = armHeaders
    .flatMap((arm) => [...arm.matchAll(/"([a-z][a-z_]*)"/g)].map((m) => m[1]))
    .sort();

  const HANDLER_KEYS = ["command", "env", "timeout", "type"];
  const GROUP_KEYS = ["hooks", "matcher"];

  function validateAgyHooks(candidate: Record<string, Record<string, AgyEntry[]>>) {
    expect(Object.keys(candidate)).toEqual(["gitpulse"]);
    expect(Object.keys(candidate.gitpulse).sort()).toEqual([
      "PostInvocation",
      "PostToolUse",
      "PreInvocation",
      "PreToolUse",
      "SessionStart",
      "Stop",
    ]);
    expect(implemented.size).toBeGreaterThan(0);
    expect(notifyEvents.size).toBeGreaterThan(0);
    expect(agyEditTools.length).toBeGreaterThan(0);

    const groupedEvents = new Set(["PreToolUse", "PostToolUse"]);
    const routes: Array<string> = [];

    for (const [event, entries] of Object.entries(candidate.gitpulse)) {
      expect(entries.length, `${event} has no entries`).toBeGreaterThan(0);
      for (const item of entries) {
        if (groupedEvents.has(event)) {
          expect(Object.keys(item).sort(), `${event} group has extra or missing keys`).toEqual(
            GROUP_KEYS,
          );
          expect(Array.isArray(item.hooks), `${event} must use grouped hooks wrapper`).toBe(true);
          expect(typeof item.matcher, `${event} group must declare a matcher`).toBe("string");
          const matcher = String(item.matcher);
          if (event === "PreToolUse") {
            expect(matcher.startsWith("^") && matcher.endsWith("$"), matcher).toBe(true);
          } else {
            expect(matcher).toBe("*");
          }
        } else {
          expect(item.hooks, `${event} is a flat lifecycle event in agy`).toBeUndefined();
          expect(item.matcher, `${event} ignores matchers in agy`).toBeUndefined();
        }

        const handlers = Array.isArray(item.hooks) ? item.hooks : [item];
        expect(handlers.length, `${event} group has no handlers`).toBeGreaterThan(0);
        for (const handler of handlers) {
          expect(
            Object.keys(handler).sort(),
            `${event} handler has unsupported or missing fields`,
          ).toEqual(HANDLER_KEYS);
          expect(handler.type, `${event}`).toBe("command");
          expect(typeof handler.command, `${event}`).toBe("string");
          const rawCommand = String(handler.command);
          expect(rawCommand, `${event}: untrimmed command`).toBe(rawCommand.trim());
          expect(
            rawCommand.startsWith(WORKTREE_LAUNCHER_PREFIX),
            `${event}: Claude worktree launcher is invalid in agy`,
          ).toBe(false);
          expect(
            rawCommand.includes("$"),
            `${event}: uninterpolated shell variable in ${rawCommand}`,
          ).toBe(false);

          const [bin, ...rest] = rawCommand.split(/\s+/);
          expect(bin, `${event}`).toBe("gitpulse-hook");
          expect(rest.length, `${event}: ${rawCommand}`).toBeGreaterThan(0);
          const sub = rest[0];
          expect(implemented, `${event}: ${sub}`).toContain(sub);
          if (sub === "notify") {
            expect(rest.length, `${event}: ${rawCommand} requires an event name`).toBe(2);
            expect(notifyEvents, `${event}: unknown notify event ${rest[1]}`).toContain(rest[1]);
          } else {
            expect(rest.length, `${event}: ${rawCommand} takes no second argument`).toBe(1);
          }

          // Antigravity's `hooks.json` contract (`HookHandlerConfig.timeout` int32)
          // measures `timeout` in whole seconds (default 30), NOT milliseconds.
          expect(typeof handler.timeout, `${event}: ${rawCommand} has no timeout`).toBe("number");
          const timeout = handler.timeout as number;
          expect(Number.isInteger(timeout), `${event}: ${rawCommand} timeout must be an int`).toBe(true);
          expect(timeout, `${event}: ${rawCommand}`).toBeGreaterThan(0);
          expect(timeout, `${event}: ${rawCommand}`).toBeLessThanOrEqual(60);

          const minTimeout = minHostTimeoutSeconds(sub, rest[1] ?? "");
          expect(
            timeout,
            `${event} "${rawCommand}" times out at ${timeout}s before its ${minTimeout}s internal budget + margin`,
          ).toBeGreaterThanOrEqual(minTimeout);

          // `agy` natively injects `PLUGIN_ROOT` into plugin hook subprocesses
          // and does NOT interpolate `$PLUGIN_ROOT` in JSON `env` maps.
          expect(handler.env, `${event}: ${rawCommand}`).toEqual({
            GITPULSE_HOST: "antigravity",
          });

          const matcherKey = groupedEvents.has(event) ? String(item.matcher) : "";
          routes.push(`${event}|${matcherKey}|${rawCommand}`);
        }
      }
    }

    const editMatcher = String(candidate.gitpulse.PreToolUse[0]?.matcher ?? "");
    const editGroupMatch = /^\^\(([a-z_|]+)\)\$$/.exec(editMatcher);
    expect(editGroupMatch, `invalid edit matcher ${editMatcher}`).toBeTruthy();
    const declaredEditTools = editGroupMatch![1].split("|");
    expect(new Set(declaredEditTools).size, "duplicate edit tool in matcher").toBe(
      declaredEditTools.length,
    );
    expect([...declaredEditTools].sort()).toEqual(agyEditTools);

    expect(routes).toEqual([
      `PreToolUse|${editMatcher}|gitpulse-hook collision-guard`,
      "PreToolUse|^run_command$|gitpulse-hook command-gate",
      "PostToolUse|*|gitpulse-hook notify tool_finished",
      "SessionStart||gitpulse-hook session-brief",
      "SessionStart||gitpulse-hook peer-inbox",
      "PreInvocation||gitpulse-hook peer-inbox",
      "PostInvocation||gitpulse-hook peer-inbox",
      "Stop||gitpulse-hook stop-or-notify",
    ]);
  }

  it("carries a valid Antigravity manifest, logo asset, MCP config, and named-hook root hooks.json", () => {
    const manifest = loadJsonAt(path.join(AGY_PLUGIN, "plugin.json"));
    expect(manifest.name).toBe(path.basename(AGY_PLUGIN));
    expect(manifest.logo).toBe("assets/logo.svg");
    const logoPath = path.resolve(AGY_PLUGIN, String(manifest.logo));
    expect(logoPath.startsWith(AGY_PLUGIN + path.sep)).toBe(true);
    expect(existsSync(logoPath)).toBe(true);
    expect(readFileSync(logoPath, "utf8").trimStart().startsWith("<svg")).toBe(true);

    const mcp = loadJsonAt(path.join(AGY_PLUGIN, "mcp_config.json")) as Record<string, unknown>;
    expect(Object.keys(mcp)).toEqual(["mcpServers"]);
    const servers = mcp.mcpServers as Record<string, Record<string, unknown>>;
    expect(Object.keys(servers)).toEqual(["gitpulse"]);
    expect(servers.gitpulse).toEqual({
      command: "gitpulse-mcp",
      args: [],
      env: { GITPULSE_HOST: "antigravity" },
    });

    validateAgyHooks(hooks);
  });

  it.runIf(gitAvailable)("keeps every file in plugins/gitpulse-agy committable", () => {
    const files: string[] = [];
    const walk = (current: string) => {
      for (const name of readdirSync(current)) {
        const full = path.join(current, name);
        if (statSync(full).isDirectory()) walk(full);
        else files.push(full);
      }
    };
    walk(AGY_PLUGIN);
    expect(files.length).toBeGreaterThan(0);
    const ignored = files.filter(isIgnored).map((file) => path.relative(ROOT, file));
    expect(ignored, `these would not survive a clone: ${ignored.join(", ")}`).toEqual([]);
  });

  it("rejects millisecond, zero, negative, fractional, missing, or below-budget timeouts, invalid matchers, and uninterpolated env vars", () => {
    // Boundary acceptance: 60s (the synchronous ceiling) is valid.
    const atCeiling = structuredClone(hooks);
    atCeiling.gitpulse.PreToolUse[0].hooks![0].timeout = 60;
    expect(() => validateAgyHooks(atCeiling)).not.toThrow();

    // Invalid timeouts: missing, string, 0, negative, float, >60s, millisecond values, and below internal budget.
    for (const badTimeout of [undefined, "10", 0, -5, 5.5, 61, 5000, 10000, 9]) {
      const copy = structuredClone(hooks);
      copy.gitpulse.PreToolUse[0].hooks![0].timeout = badTimeout;
      expect(() => validateAgyHooks(copy), `collision-guard timeout=${String(badTimeout)}`).toThrow();
    }
    const belowPeerBudget = structuredClone(hooks);
    belowPeerBudget.gitpulse.PreInvocation[0].timeout = 3;
    expect(() => validateAgyHooks(belowPeerBudget)).toThrow();

    // `notify tool_finished` waits 1.5s read + 1.5s send = 3.0s, plus 2s host margin = 5s.
    for (const badNotifyTimeout of [3, 4]) {
      const belowNotifyBudget = structuredClone(hooks);
      belowNotifyBudget.gitpulse.PostToolUse[0].hooks![0].timeout = badNotifyTimeout;
      expect(
        () => validateAgyHooks(belowNotifyBudget),
        `notify tool_finished timeout=${badNotifyTimeout}`,
      ).toThrow();
    }

    // `stop-or-notify` waits 2.0s read + 0.8s send + 2.0s host margin = 4.8s -> integer >= 5s.
    const belowStopOrNotifyBudget = structuredClone(hooks);
    belowStopOrNotifyBudget.gitpulse.Stop[0].timeout = 4;
    expect(() => validateAgyHooks(belowStopOrNotifyBudget), "stop-or-notify timeout=4").toThrow();

    // `notify` with missing argument or unknown bridge event.
    for (const badNotifyCmd of ["gitpulse-hook notify", "gitpulse-hook notify unknown_event"]) {
      const copy = structuredClone(hooks);
      copy.gitpulse.PostToolUse[0].hooks![0].command = badNotifyCmd;
      expect(() => validateAgyHooks(copy), badNotifyCmd).toThrow();
    }

    // Claude worktree launcher or uninterpolated `$` variable in `command`.
    for (const badCommand of [
      `${WORKTREE_LAUNCHER_PREFIX}gitpulse-hook collision-guard`,
      '"$PLUGIN_ROOT/gitpulse-hook" collision-guard',
    ]) {
      const copy = structuredClone(hooks);
      copy.gitpulse.PreToolUse[0].hooks![0].command = badCommand;
      expect(() => validateAgyHooks(copy), badCommand).toThrow();
    }

    // Unsupported extra fields on handler (`async`, `asyncRewake`, `timeoutMs`) or group wrapper.
    for (const extraField of ["async", "asyncRewake", "timeoutMs"]) {
      const copy = structuredClone(hooks);
      (copy.gitpulse.PostToolUse[0].hooks![0] as Record<string, unknown>)[extraField] = true;
      expect(() => validateAgyHooks(copy), `handler extra field ${extraField}`).toThrow();
    }
    const extraOnGroup = structuredClone(hooks);
    extraOnGroup.gitpulse.PostToolUse[0].timeout = 5;
    expect(() => validateAgyHooks(extraOnGroup), "group wrapper extra timeout").toThrow();

    // Deleting a safety gate (`command-gate`), startup brief (`session-brief`), or retargeting `Stop`.
    const deletedCommandGate = structuredClone(hooks);
    deletedCommandGate.gitpulse.PreToolUse.splice(1, 1);
    expect(() => validateAgyHooks(deletedCommandGate), "deleted command-gate").toThrow();

    const deletedSessionBrief = structuredClone(hooks);
    deletedSessionBrief.gitpulse.SessionStart.splice(0, 1);
    expect(() => validateAgyHooks(deletedSessionBrief), "deleted session-brief").toThrow();

    const retargetedStop = structuredClone(hooks);
    retargetedStop.gitpulse.Stop[0].command = "gitpulse-hook peer-inbox";
    expect(() => validateAgyHooks(retargetedStop), "retargeted Stop to peer-inbox").toThrow();

    const driftedEditMatcher = structuredClone(hooks);
    driftedEditMatcher.gitpulse.PreToolUse[0].matcher = "^(write_file|edit_file)$";
    expect(() => validateAgyHooks(driftedEditMatcher), "dropped edit tools from matcher").toThrow();

    // Uninterpolated `$PLUGIN_ROOT` or missing `GITPULSE_HOST` in `env`.
    const uninterpolatedEnv = structuredClone(hooks);
    uninterpolatedEnv.gitpulse.Stop[0].env = {
      GITPULSE_HOST: "antigravity",
      GITPULSE_AGY_PLUGIN_ROOT: "$PLUGIN_ROOT",
    };
    expect(() => validateAgyHooks(uninterpolatedEnv)).toThrow();

    const emptyEnv = structuredClone(hooks);
    emptyEnv.gitpulse.Stop[0].env = {};
    expect(() => validateAgyHooks(emptyEnv)).toThrow();

    // Missing or invalid matchers on grouped vs flat events.
    for (const badPostMatcher of [undefined, ""]) {
      const copy = structuredClone(hooks);
      copy.gitpulse.PostToolUse[0].matcher = badPostMatcher;
      expect(() => validateAgyHooks(copy), `PostToolUse matcher=${String(badPostMatcher)}`).toThrow();
    }
    const unanchoredPre = structuredClone(hooks);
    unanchoredPre.gitpulse.PreToolUse[1].matcher = "run_command";
    expect(() => validateAgyHooks(unanchoredPre)).toThrow();

    const flatWithMatcher = structuredClone(hooks);
    flatWithMatcher.gitpulse.Stop[0].matcher = "*";
    expect(() => validateAgyHooks(flatWithMatcher)).toThrow();

    // Empty event or handler arrays.
    const emptyGroup = structuredClone(hooks);
    emptyGroup.gitpulse.PostToolUse[0].hooks = [];
    expect(() => validateAgyHooks(emptyGroup)).toThrow();

    const emptyEvent = structuredClone(hooks);
    emptyEvent.gitpulse.Stop = [];
    expect(() => validateAgyHooks(emptyEvent)).toThrow();
  });
});
