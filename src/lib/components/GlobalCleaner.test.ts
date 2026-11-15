import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "GlobalCleaner.svelte"),
  "utf8",
);

const refreshBody = () => {
  const start = source.indexOf("async function refresh()");
  return source.slice(start, source.indexOf("function edited()", start));
};

describe("GlobalCleaner status read failures", () => {
  it("keeps the status-read error apart from action errors", () => {
    expect(source).toContain("let refreshError = $state<string | null>(null);");
    const body = refreshBody();
    // A poll writes only refreshError: it must neither set nor clear a
    // save/run/inspect error the reader has not seen yet.
    expect(body).toContain("refreshError = formatError(e)");
    expect(body).toContain("refreshError = null;");
    expect(body).not.toMatch(/\berror = /);
  });

  it("clears the read error only on a successful read", () => {
    const body = refreshBody();
    const ok = body.slice(body.indexOf("accept(next)"), body.indexOf("} catch"));
    expect(ok).toContain("refreshError = null;");
    expect(body.slice(body.indexOf("} catch"))).not.toContain("refreshError = null");
  });

  it("shows both errors in an alert", () => {
    expect(source).toContain('{#if error}<p role="alert"');
    expect(source).toContain('{#if refreshError}<p role="alert"');
  });

  it("offers Try again instead of Loading forever when the first read failed", () => {
    const at = source.indexOf("{:else if refreshError}");
    expect(at).toBeGreaterThan(-1);
    const loading = source.indexOf("Loading saved cleanup policy…");
    expect(loading).toBeGreaterThan(at);
    const failed = source.slice(at, loading);
    expect(failed).toContain("Couldn't read the cleanup policy.");
    expect(failed).toContain("onclick={refresh}");
    expect(failed).toContain("Try again");
  });

  it("formats every rejection with formatError, never String(e)", () => {
    expect(source).toContain('import { formatError } from "../ui/formatError";');
    expect(source).not.toContain("String(e)");
    expect(source.match(/formatError\(e\)/g)?.length).toBe(7);
  });
});

describe("GlobalCleaner reclaim scopes and clone budget", () => {
  it("offers the three opt-in scopes and the idle period, each reading a missing scopes as the default", () => {
    expect(source).toContain("Reclaim more (off by default)");
    for (const label of [
      "Reclaim build caches in idle linked worktrees",
      "Judge uncommitted work per directory, not per repository",
      "Remove stale linked worktrees",
    ]) expect(source).toContain(`ariaLabel="${label}"`);
    for (const key of ["worktree_caches", "per_tree_vetoes", "prune_worktrees", "worktree_idle_days"]) expect(source).toContain(`"${key}"`);
    expect(source).toContain("scopesOf(draft)");
    expect(source).toContain("[7, 14, 30, 90]");
  });

  it("warns that no run uses a scope until the saved set was previewed", () => {
    expect(source).toContain("anyScopeOn(scopesOf(cleaner.config)) && !cleaner.scopes_previewed");
    expect(source).toContain("{#if scopesAwaitPreview}");
    expect(source).toContain("until you inspect saved roots");
  });

  it("saves scopes and the clone budget through the draft, edited and cmd_cleaner_save flow", () => {
    expect(source).toContain("draft.scopes = { ...scopesOf(draft), [key]: value }; edited();");
    expect(source).toContain("draft.clone_budget = { ...cloneBudgetOf(draft), [key]: value }; edited();");
    expect(source).toContain('invoke<CleanerState>("cmd_cleaner_save", { config })');
  });

  it("offers the clone budget choices, with no limit as 0", () => {
    expect(source).toContain("Worktrees that may hold a cloned Cargo target");
    expect(source).toContain("[2, 4, 8, 16]");
    expect(source).toContain("Advisory total for linked worktrees' private bytes");
    expect(source).toContain("[0, 50 * GIB, 100 * GIB, 200 * GIB]");
  });

  it("tags each inventory row with its scope and shows what removal frees when known", () => {
    expect(source).toContain("scopeLabel(row.scope)");
    expect(source).toContain("{#if row.private_bytes !== undefined}");
    expect(source).toContain("frees {humanBytes(row.private_bytes)}");
  });
});
