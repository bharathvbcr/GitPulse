import { mkdirSync, mkdtempSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { createLogger, createServer, type ViteDevServer } from "vite";
import { afterEach, describe, expect, it } from "vitest";
import { isNestedAgentWorktree } from "./vite-watch.mjs";

const repo = join(dirname(fileURLToPath(import.meta.url)), "..");
const TSCONFIG_RELOAD = "changed tsconfig file detected";

/** Polls `seen` until it holds, failing loudly at the deadline. */
async function waitFor(seen: () => boolean, what: string, budgetMs: number): Promise<number> {
  const started = Date.now();
  while (!seen()) {
    if (Date.now() - started > budgetMs) throw new Error(`${what} not observed within ${budgetMs}ms`);
    await new Promise(resolve => setTimeout(resolve, 25));
  }
  return Date.now() - started;
}

const servers: ViteDevServer[] = [];
const dirs: string[] = [];

afterEach(async () => {
  await Promise.all(servers.splice(0).map(server => server.close()));
  for (const dir of dirs.splice(0)) rmSync(dir, { recursive: true, force: true });
});

describe("isNestedAgentWorktree", () => {
  const root = "/repo";

  it("matches every agent's worktree container and what is inside it", () => {
    for (const file of [
      "/repo/.claude/worktrees",
      "/repo/.claude/worktrees/merge-tasks/tsconfig.json",
      "/repo/.codex/worktrees/x/src/App.svelte",
      "/repo/.cursor/worktrees/y",
      "/repo/.gitpulse/worktrees/import-0b7d4e2a/package.json",
    ]) expect(isNestedAgentWorktree(root, file), file).toBe(true);
  });

  it("leaves the checkout's own files, git metadata and outside paths alone", () => {
    for (const file of [
      "/repo",
      "/repo/tsconfig.json",
      "/repo/src/lib/work/agentWorktree.ts",
      "/repo/.claude/settings.json",
      "/repo/.gitpulse/hooks.toml",
      "/repo/docs/worktrees/notes.md",
      "/repo/.git/worktrees/merge-tasks/HEAD",
      "/elsewhere/.claude/worktrees/x/tsconfig.json",
      "/repo-sibling/.claude/worktrees/x",
    ]) expect(isNestedAgentWorktree(root, file), file).toBe(false);
  });

  it("reads the layout below the root, not in the root's own path", () => {
    const self = "/repo/.claude/worktrees/merge-tasks";
    expect(isNestedAgentWorktree(self, `${self}/src/App.svelte`)).toBe(false);
    expect(isNestedAgentWorktree(self, `${self}/tsconfig.json`)).toBe(false);
    expect(isNestedAgentWorktree(self, `${self}/.claude/worktrees/inner/tsconfig.json`)).toBe(true);
  });
});

// A sibling session creating `.claude/worktrees/<slug>` inside this checkout
// put a tsconfig.json under the dev server's root, and vite answered with a
// forced full reload. A harness page reloaded mid-run restarts `?check=1`, so
// the verdict that reached ci:local was about a page nobody finished testing.
// vite 8 drives that reload (`reloadOnTsconfigChange`) from the one chokidar
// watcher whose only filter is `server.watch.ignored`, so these tests start
// the real configs and watch for the reload itself rather than reading the
// ignore list back.
describe.each(["vite.config.ts", "vite.harness.config.ts"])("%s file watcher", configName => {
  it("does not reload when a nested agent worktree gains a tsconfig.json", async () => {
    const scratch = realpathSync(mkdtempSync(join(tmpdir(), "gp-vite-watch-")));
    dirs.push(scratch);
    // The checkout under test is itself an agent worktree, as every sibling
    // session's is. The controls below then also prove the exclusion did not
    // switch off the watcher for a checkout whose absolute path has the layout.
    const root = join(scratch, ".codex", "worktrees", "self");
    mkdirSync(root, { recursive: true });
    writeFileSync(join(root, "index.html"), "<!doctype html><title>watch</title>");
    const lines: string[] = [];
    const logger = createLogger("info", { allowClearScreen: false });
    logger.info = message => { lines.push(message); };
    const server = await createServer({
      configFile: join(repo, configName),
      root,
      cacheDir: join(root, ".vite-cache"),
      customLogger: logger,
      server: { port: 0, strictPort: false },
    });
    servers.push(server);
    // The plugin adds to the config's own list; it must not replace it.
    const ignored = [server.config.server.watch?.ignored ?? []].flat();
    expect(ignored.some(entry => typeof entry === "function")).toBe(true);
    if (configName === "vite.config.ts") expect(ignored).toContain("**/src-tauri/**");
    const reloadsFor = (file: string) =>
      lines.filter(line => line.includes(TSCONFIG_RELOAD) && line.includes(file)).length;

    // Positive control: the watcher is live and reaches tsconfig detection.
    // Rewritten until seen, because chokidar's `ready` can fire before
    // createServer returns and an initial write may land before the scan.
    const control = join(root, "tsconfig.json");
    let latency = 0;
    for (let attempt = 0; reloadsFor(control) === 0; attempt += 1) {
      if (attempt === 40) throw new Error(`control reload never observed; log: ${JSON.stringify(lines)}`);
      writeFileSync(control, JSON.stringify({ attempt }));
      try { latency = await waitFor(() => reloadsFor(control) > 0, "control reload", 250); } catch { /* rewrite */ }
    }

    // What the sibling session did: a fresh worktree directory, created after
    // the server started, carrying its own tsconfig.json.
    const nestedDir = join(root, ".claude", "worktrees", "merge-tasks");
    mkdirSync(nestedDir, { recursive: true });
    const nested = join(nestedDir, "tsconfig.json");
    writeFileSync(nested, "{}");

    // A second control written after the nested file: once it is seen, the
    // watcher has had at least as long as the nested write needed, plus a
    // settle window scaled by the latency this host just showed.
    const later = join(root, "packages", "tsconfig.json");
    mkdirSync(dirname(later), { recursive: true });
    writeFileSync(later, "{}");
    await waitFor(() => reloadsFor(later) > 0, "second control reload", 10_000);
    await new Promise(resolve => setTimeout(resolve, Math.max(500, latency * 4)));

    expect(reloadsFor(nested), JSON.stringify(lines)).toBe(0);
    const watchedDirs = Object.keys(server.watcher.getWatched());
    expect(watchedDirs.filter(dir => dir.startsWith(join(root, ".claude", "worktrees")))).toEqual([]);
  }, 30_000);
});
