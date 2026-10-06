import path from "node:path";
import { agentLayout } from "../src/lib/work/agentWorktree.ts";

/**
 * True when `file` sits in an agent worktree nested under `root`:
 * `<root>/.<agent>/worktrees/…`, the layout Claude Code, Cursor, Codex and
 * GitPulse's own task lanes (`.gitpulse/worktrees/`) create. The layout rule
 * is `agentLayout`'s, not a second copy of it.
 *
 * Matched on the path relative to `root`, never on the absolute path. A
 * checkout that is itself an agent worktree (`…/.claude/worktrees/x/`) has
 * that layout in every absolute path it owns, and a pattern like
 * `**\/.*\/worktrees/**` would stop its dev server watching anything at all.
 *
 * @param {string} root
 * @param {string} file
 * @returns {boolean}
 */
export function isNestedAgentWorktree(root, file) {
  const relative = path.relative(root, file);
  if (!relative || relative.startsWith("..") || path.isAbsolute(relative)) return false;
  return agentLayout(relative) !== null;
}

/**
 * Keeps vite's file watcher out of sibling sessions' worktrees.
 *
 * Another session's worktree is a whole checkout inside this one, and vite
 * reacts to what happens there: a tsconfig.json created or edited anywhere
 * under the root forces a full reload of every page (`reloadOnTsconfigChange`
 * in vite 8, fired from the watcher's add/change/unlink handlers and sent
 * straight to the clients, so `server.hmr: false` does not stop it). A
 * browser harness reloaded mid-run restarts `?check=1` and reports on a page
 * nobody finished checking.
 *
 * The watcher's only filter is `server.watch.ignored`, so that is where this
 * goes. It is a plugin rather than a literal because the anchor has to be the
 * root vite will actually watch, which callers such as the browser-regression
 * runner set inline; vite concatenates arrays when it merges configs, so this
 * adds to a config's own `ignored` list instead of replacing it.
 *
 * @returns {import("vite").Plugin}
 */
export function gitpulseIgnoreNestedWorktrees() {
  return {
    name: "gitpulse-ignore-nested-worktrees",
    config(userConfig) {
      const root = path.resolve(userConfig.root ?? process.cwd());
      return {
        server: { watch: { ignored: [(/** @type {string} */ file) => isNestedAgentWorktree(root, file)] } },
      };
    },
  };
}
