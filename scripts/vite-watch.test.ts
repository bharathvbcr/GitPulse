import { mkdirSync, mkdtempSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { createLogger, createServer, type ViteDevServer } from "vite";
import { afterEach, describe, expect, it } from "vitest";
import { isNestedAgentWorktree } from "./vite-watch.mjs";

const repo = join(dirname(fileURLToPath(import.meta.url)), "..");
const TSCONFIG_RELOAD = "changed tsconfig file detected";

/** Vite and chokidar report every path with `/`, whatever the platform joins with. */
const posix = (file: string) => file.split(sep).join("/");

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
    // `.native`: Windows runners hand out an 8.3 short temp path
    // (`RUNNER~1`), which only the native realpath expands to the long name
    // vite reports, so the log lines would never match the controls.
    const scratch = realpathSync.native(mkdtempSync(join(tmpdir(), "gp-vite-watch-")));
    dirs.push(scratch);
    // The checkout under test is itself an agent worktree, as every sibling
    // session's is. The controls below then also prove the exclusion did not
    // switch off the watcher for a checkout whose absolute path has the layout.
    const root = join(scratch, ".codex", "worktrees", "self");
    mkdirSync(root, { recursive: true });
    writeFileSync(join(root, "index.html"), "<!doctype html><title>watch</title>");
    const lines: string[] = [];
    // Every logged line re-checks the pending waits, so a control is awaited
    // by the event that proves it rather than by a clock. Delivery latency of
    // the OS file-event stream is the host's, and an overloaded host has shown
    // more than the ten seconds a fixed attempt budget allowed; the test's own
    // timeout is the only deadline.
    const pending = new Set<() => void>();
    const logger = createLogger("info", { allowClearScreen: false });
    logger.info = message => {
      lines.push(message);
      for (const check of pending) check();
    };
    const until = (seen: () => boolean) =>
      new Promise<void>(resolve => {
        const check = () => {
          if (!seen()) return;
          pending.delete(check);
          resolve();
        };
        pending.add(check);
        check();
      });
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
      lines.filter(line => line.includes(TSCONFIG_RELOAD) && line.includes(posix(file))).length;

    // Positive control: the watcher is live and reaches tsconfig detection.
    // Rewritten until seen, because chokidar's `ready` can fire before
    // createServer returns and an initial write may land before the scan.
    // The rewrite interval paces the writes; it does not bound the wait.
    const control = join(root, "tsconfig.json");
    const controlSeen = until(() => reloadsFor(control) > 0);
    let controlObserved = false;
    void controlSeen.then(() => { controlObserved = true; });
    for (let attempt = 0; !controlObserved; attempt += 1) {
      writeFileSync(control, JSON.stringify({ attempt }));
      await Promise.race([controlSeen, new Promise(resolve => setTimeout(resolve, 250))]);
    }

    // What the sibling session did: a fresh worktree directory, created after
    // the server started, carrying its own tsconfig.json.
    const nestedDir = join(root, ".claude", "worktrees", "merge-tasks");
    mkdirSync(nestedDir, { recursive: true });
    const nested = join(nestedDir, "tsconfig.json");
    writeFileSync(nested, "{}");

    // A second control written after the nested file: once it is seen, the
    // watcher has had at least as long as the nested write needed, plus a
    // settle window as long as the delivery just measured. The first control's
    // wait is not that measure: it includes the event stream starting up.
    const later = join(root, "packages", "tsconfig.json");
    mkdirSync(dirname(later), { recursive: true });
    const laterWritten = Date.now();
    writeFileSync(later, "{}");
    await until(() => reloadsFor(later) > 0);
    await new Promise(resolve => setTimeout(resolve, Math.max(500, Date.now() - laterWritten)));

    expect(reloadsFor(nested), JSON.stringify(lines)).toBe(0);
    const watchedDirs = Object.keys(server.watcher.getWatched());
    expect(watchedDirs.filter(dir => posix(dir).startsWith(posix(join(root, ".claude", "worktrees"))))).toEqual([]);
  }, 30_000);
});
