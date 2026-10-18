import { readdirSync, readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { classifierMatches } from "./error-classifier-scan";

/**
 * A backend error crosses IPC as `{ kind, message, detail? }`, and the one
 * place that reads it is `src/lib/ipc/invoke.ts`. Anything else that searches
 * an error's text, or compares it with a phrase, is a classifier that breaks
 * silently when a Rust message is reworded, or wrapped by a caller that
 * prefixes it, and (for a message that quotes repository content) one the
 * repository can write to.
 *
 * `error-classifier-scan.ts` finds those by where a value comes from (a catch
 * binding, `.message`, `String(x)`, `formatError(x)`, and what is derived from
 * them) and not by what it is called. Every match outside the guard must be in
 * one of the two lists below, **by its exact source line**, so a new `includes`
 * in a listed file is a new failure. The lists can only shrink: an entry whose
 * line stops matching fails too. What the scan cannot see is named in
 * error-classifier-evasion.test.ts.
 */
const SRC = fileURLToPath(new URL("../src/", import.meta.url));
const GUARD = "lib/ipc/invoke.ts";

interface Entry {
  file: string;
  /** The source line, trimmed, exactly as the scan reports it. */
  line: string;
  /** How many matches that line holds. */
  count?: number;
  why: string;
}

/** Matches that do not classify a backend error. */
const NOT_A_CLASSIFIER: readonly Entry[] = [
  { file: "lib/components/CommitRow.svelte", line: "const match = msg.match(/^([a-zA-Z]+)(\\([^\\)]+\\))?(!)?:\\s/);", why: "parses a commit subject's conventional-commit prefix, which is data the user wrote" },
  { file: "lib/diagnostics/diagnostics.ts", line: "return /effect_update_depth_exceeded/i.test(message)", why: "recognises the webview's own console noise, not a backend error" },
  { file: "lib/diagnostics/diagnostics.ts", line: "|| /svelte\\.dev\\/e\\/effect_update_depth_exceeded/i.test(message);", why: "recognises the webview's own console noise, not a backend error" },
  { file: "lib/diagnostics/diagnostics.ts", line: "if (message.includes(\"read and wrote the same state\")) return message;", why: "recognises the webview's own console noise, not a backend error" },
  { file: "lib/diagnostics/diagnostics.ts", line: "if (text.startsWith(\"[TAURI] Couldn't find callback id \")) return true;", why: "recognises the Tauri runtime's console noise" },
  { file: "lib/diagnostics/diagnostics.ts", line: "text.includes(", why: "recognises the Tauri runtime's console noise (the IPC custom protocol fallback)" },
  { file: "lib/diagnostics/diagnostics.ts", line: "if (text.startsWith(\"[hmr] Failed to reload \")) return true;", why: "recognises the dev server's console noise" },
  { file: "lib/diagnostics/diagnostics.ts", line: "return /^ResizeObserver loop (?:limit exceeded|completed with undelivered notifications)\\.?$/.test(text);", why: "recognises the browser's own console noise" },
  { file: "lib/diff/wordDiff.ts", line: "if (line.startsWith(\"Binary files \") || line.startsWith(\"GIT binary patch\")) return \"binary\";", why: "reads the header lines of a diff, which is the content being shown" },
  { file: "lib/firebase/rolloutState.ts", line: "sublabel: displayText((rollout.commit?.message ?? \"\").split(\"\\n\")[0]),", why: "takes the first line of a commit message to show" },
  { file: "lib/platform.ts", line: "return /Mac/.test(platform) || /Macintosh|Mac OS X/.test(ua);", why: "reads the browser's user agent, not an error" },
  { file: "lib/pulse/metrics.ts", line: "const newline = message.indexOf(\"\\n\");", why: "finds the end of a commit subject line" },
  { file: "lib/repos/gitPreflight.ts", line: "const at = e.lastIndexOf(\"@\");", why: "validates an email address the user typed (`e` is the email)" },
  { file: "lib/repos/gitPreflight.ts", line: "if (e.length > 320 || /[\\s<>\\u0000-\\u001f\\u007f]/.test(e) || at <= 0 || at === e.length - 1) {", why: "validates an email address the user typed (`e` is the email)" },
  { file: "lib/repos/paths.ts", line: "if (/Mac|Macintosh|Win/i.test(platform) || /Mac OS X|Windows/i.test(ua)) {", why: "reads the browser's platform and user agent, not an error" },
  { file: "lib/stores/mutationScan.ts", line: "if (isRange(inner) && !templateType.test(String((inner as { type?: unknown }).type))) {", why: "tests a syntax-tree node's type name in a source scan" },
  { file: "lib/stores/repoStore.ts", line: "runMutating(\"commit\", message.split(\"\\n\")[0].slice(0, 80), (path) =>", count: 2, why: "takes the first line of the commit message the user typed, for the activity label" },
  { file: "lib/stores/sourceScan.ts", line: "const next = rest.search(/\\n  (?:async )?function \\w+\\(|\\n<\\/script>/);", why: "searches source text for the next function, in a source scan" },
];

/**
 * Backend text still read for a phrase. Each is a command error, a report
 * struct or a third party's output that has no kind to carry yet, so it is a
 * follow-up, named here so it is not forgotten and cannot grow.
 */
const REMAINING_TEXT_READERS: readonly Entry[] = [
  { file: "lib/async/deferral.ts", line: "return message.includes(DEFERRED_UNDER_LOAD_MARKER);", why: "text helper for already-flattened strings (toast severity, diagnostics severity, autoInit reports, githubAlerts report fields) that carry no kind; pinned to the Rust marker by deferral.test.ts. The command-error retry in invoke.ts does not use it" },
  { file: "lib/async/deferral.ts", line: "return message.includes(RUN_TIMEOUT_MARKER) && !message.includes(SLOT_WAIT_SUFFIX);", count: 2, why: "outcomeUnknown reads the `create issue` outcome struct's error text (workbench/taskIssue.ts); that struct has no kind field yet" },
  { file: "lib/branches/deleteEscalation.ts", line: "if (text.includes(WORKTREE_GUARD) || text.includes(GIT_WORKTREE_REFUSAL)) {", count: 2, why: "reads git's own refusal wording for `branch -d` plus a Rust guard; typing it means the backend classifying git's stderr into branch-delete kinds, which no card has asked for yet" },
  { file: "lib/branches/deleteEscalation.ts", line: "if (text.includes(DEFAULT_BRANCH_GUARD)) {", why: "a Rust guard's wording on a branch delete refusal; see the entry above" },
  { file: "lib/branches/deleteEscalation.ts", line: "if (text.includes(UNMERGED_MARKER)) {", why: "git's own wording for an unmerged branch; see the entry above" },
  { file: "lib/coverage/report.ts", line: "/No such file or directory \\(os error 2\\)/i.test(output) ||", why: "classifies the output of a coverage command the user's tooling ran; third-party text with no typed channel" },
  { file: "lib/coverage/report.ts", line: "/Failed to spawn \\S+: no such program:/i.test(output)", why: "classifies a coverage command's output for the runner's spawn refusal; the Rust runner has no kind for it (SpawnNotFound was dropped as unconsumed)" },
  { file: "lib/coverage/report.ts", line: "if (/outside the purpose-specific command allowlist/i.test(output)) {", why: "classifies a coverage command's output for the allowlist refusal; third-party-shaped text with no typed channel" },
  { file: "lib/coverage/report.ts", line: "/npx canceled due to missing packages/i.test(output) ||", why: "classifies npx's own output" },
  { file: "lib/coverage/report.ts", line: "/no YES option/i.test(output)", why: "classifies npx's own output" },
  { file: "lib/coverage/report.ts", line: "/Cannot find dependency '@vitest\\/coverage-v8'/i.test(output) ||", why: "classifies vitest's own output" },
  { file: "lib/coverage/report.ts", line: "if (/wrapper ['`].*['`] is not a repository file/i.test(output)) {", why: "classifies the command gate's refusal text for a coverage run; no typed channel" },
  { file: "lib/health/githubAlerts.ts", line: "text.includes(\"code scanning is not enabled\") ||", why: "reads `gh api`'s own wording to tell a disabled product from a failure; `unavailable_reason` on the report is the typed channel and this is its fallback" },
  { file: "lib/health/githubAlerts.ts", line: "text.includes(\"code scanning is disabled\") ||", why: "`gh api` wording; see the first githubAlerts entry" },
  { file: "lib/health/githubAlerts.ts", line: "text.includes(\"advanced security must be enabled\") ||", why: "`gh api` wording; see the first githubAlerts entry" },
  { file: "lib/health/githubAlerts.ts", line: "text.includes(\"dependabot alerts are disabled\") ||", why: "`gh api` wording; see the first githubAlerts entry" },
  { file: "lib/health/githubAlerts.ts", line: "text.includes(\"dependabot alerts are not enabled\") ||", why: "`gh api` wording; see the first githubAlerts entry" },
  { file: "lib/health/githubAlerts.ts", line: "text.includes(\"dependabot is not enabled\") ||", why: "`gh api` wording; see the first githubAlerts entry" },
  { file: "lib/health/githubAlerts.ts", line: "text.includes(\"dependabot is not currently enabled\") ||", why: "`gh api` wording; see the first githubAlerts entry" },
  { file: "lib/health/githubAlerts.ts", line: "text.includes(\"no analysis found\")", why: "`gh api` wording; see the first githubAlerts entry" },
  { file: "lib/terminal/inputQueue.ts", line: "return SESSION_GONE.test(String(error));", why: "recognises the terminal host's \"no such session\" rejection of a write; those commands reject with plain strings and need a kind of their own" },
  { file: "lib/ui/formatError.ts", line: "return /\\bfile not found:/i.test(reason);", why: "reads the `File not found:` phrase of get_file_blob/get_file_content from a coverage reason string the caller already flattened; needs a kind on that command's error" },
  { file: "lib/workbench/repositoryRelink.ts", line: "if (NOT_A_REPOSITORY.test(message)) return { state: \"missing\", path, detail: `${repo.name} was not found at ${path}. It was moved or deleted; relink it to where it is now.` };", why: "reads the not-a-repository refusal of a workbench command; those commands reject with plain strings and need a kind of their own" },
  { file: "lib/workbench/taskModel.ts", line: "if (lower.includes(\"not served\") || lower.includes(\"errnotserved\") || lower.includes(\"capability is unavailable\")) {", count: 1, why: "classifies a Manvi worker's failure text for the task UI; the worker is a separate process with its own wording and no typed channel" },
];

function walk(dir: string): string[] {
  const out: string[] = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) out.push(...walk(full));
    else if (/\.(ts|svelte)$/.test(entry.name) && !/\.(test|spec)\./.test(entry.name) && !entry.name.endsWith(".d.ts")) {
      out.push(full);
    }
  }
  return out;
}

const key = (file: string, line: string) => JSON.stringify([file, line]);

/** The matches (`file, line`) that exceed what the list allows for that exact line. */
function unlistedMatches(found: Map<string, number>, allowed: readonly Entry[]): string[] {
  const listed = new Map(allowed.map((entry) => [key(entry.file, entry.line), entry.count ?? 1]));
  return [...found].filter(([k, count]) => (listed.get(k) ?? 0) < count)
    .map(([k, count]) => `${k} (${count} found, ${listed.get(k) ?? 0} listed)`);
}

describe("the list pins exact lines", () => {
  const entry: Entry = { file: "lib/a.ts", line: "if (text.includes(OLD)) {", why: "a reason long enough to count" };

  it("accepts the line that was listed", () => {
    expect(unlistedMatches(new Map([[key("lib/a.ts", entry.line), 1]]), [entry])).toEqual([]);
  });

  it("fails a new search in a file that has a listed one", () => {
    const found = new Map([[key("lib/a.ts", entry.line), 1], [key("lib/a.ts", "if (text.includes(NEW)) {"), 1]]);
    expect(unlistedMatches(found, [entry])).toHaveLength(1);
  });

  it("fails a second copy of a listed line", () => {
    expect(unlistedMatches(new Map([[key("lib/a.ts", entry.line), 2]]), [entry])).toHaveLength(1);
  });
});

describe("no error is classified by its text", () => {
  const allowed = [...NOT_A_CLASSIFIER, ...REMAINING_TEXT_READERS];
  const found = new Map<string, number>();
  for (const full of walk(SRC)) {
    const file = path.relative(SRC, full).split(path.sep).join("/");
    if (file === GUARD) continue;
    for (const match of classifierMatches(full, readFileSync(full, "utf8"))) {
      found.set(key(file, match.line), (found.get(key(file, match.line)) ?? 0) + 1);
    }
  }

  it("matches no error text outside the one guard, except the exact lines listed with their reason", () => {
    expect(unlistedMatches(found, allowed)).toEqual([]);
  });

  it("lists nothing that no longer matches, and nothing twice", () => {
    const keys = allowed.map((entry) => key(entry.file, entry.line));
    expect(keys.filter((k, i) => keys.indexOf(k) !== i), "duplicate entries").toEqual([]);
    const stale = allowed.filter((entry) => (found.get(key(entry.file, entry.line)) ?? 0) !== (entry.count ?? 1));
    expect(stale.map((entry) => `${entry.file}: ${entry.line}`)).toEqual([]);
  });

  it("gives every listed entry a real reason", () => {
    for (const entry of allowed) expect(entry.why.length, `${entry.file} ${entry.line}`).toBeGreaterThan(20);
  });

  it("does not list the guard, which is the one place allowed to read an error", () => {
    expect(allowed.filter((entry) => entry.file === GUARD)).toEqual([]);
  });
});
