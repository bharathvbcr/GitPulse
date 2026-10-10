/**
 * The words a task search compares: normalized, stemmed, and widened by a
 * small software-domain concept map.
 *
 * This is the "smart" in the board's smart ranking, stated plainly: no
 * embeddings and no model call. Words are folded (case, width, accents,
 * camelCase and snake_case), reduced to a light stem, matched against curated
 * synonym groups, and allowed a typo or two by length. `taskRank.ts` scores
 * with it; nothing here knows about tasks.
 */

/** The most words one text contributes, so a pasted log cannot stall a keystroke. */
export const MAX_TEXT_TOKENS = 256;

const COMBINING = /\p{M}+/gu;
const WORD = /[\p{L}\p{N}]+/gu;

/** Case-, width- and accent-folded text: what two spellings must agree on. */
export function fold(text: string): string {
  return text.normalize("NFKD").replace(COMBINING, "").normalize("NFKC").toLowerCase();
}

/**
 * The words of `text`, folded, with camelCase, snake_case, kebab-case and
 * digit runs split apart: `fixOAuth2_login` reads as fix, o, auth, 2, login.
 */
export function words(text: string, limit = MAX_TEXT_TOKENS): string[] {
  if (typeof text !== "string" || !text) return [];
  const split = text
    .replace(/([\p{Ll}\p{N}])(\p{Lu})/gu, "$1 $2")
    .replace(/(\p{Lu})(\p{Lu}\p{Ll})/gu, "$1 $2")
    .replace(/(\p{L})(\p{N})/gu, "$1 $2")
    .replace(/(\p{N})(\p{L})/gu, "$1 $2");
  const out: string[] = [];
  for (const match of fold(split).matchAll(WORD)) {
    out.push(match[0]);
    if (out.length >= limit) break;
  }
  return out;
}

/** Words that say nothing about which task is meant. */
export const STOPWORDS: ReadonlySet<string> = new Set([
  "a", "an", "and", "are", "as", "at", "be", "by", "for", "from", "in", "into", "is", "it", "its",
  "of", "on", "or", "the", "to", "with", "this", "that", "when", "where", "which", "should", "can",
]);

/**
 * A light suffix stem: plurals, -ing, -ed, -ly, -ment, -ation, -er.
 *
 * Deliberately conservative — it only has to agree with itself, since both
 * the query and the card go through it, and an aggressive stemmer joins words
 * that mean different things ("organ"/"organization").
 */
export function stem(word: string): string {
  let w = word;
  if (w.length <= 3 || /\d/.test(w)) return w;
  if (w.endsWith("ies") && w.length > 4) w = `${w.slice(0, -3)}y`;
  else if (/(?:ss|sh|ch|x|z)es$/.test(w) && w.length > 4) w = w.slice(0, -2);
  else if (w.endsWith("s") && !/(?:ss|us|is)$/.test(w)) w = w.slice(0, -1);
  if (w.endsWith("ation") && w.length > 7) w = w.slice(0, -5);
  else if (w.endsWith("ment") && w.length > 7) w = w.slice(0, -4);
  else if (w.endsWith("ing") && w.length > 5) w = w.slice(0, -3);
  else if (w.endsWith("ed") && w.length > 4) w = w.slice(0, -2);
  else if (w.endsWith("ly") && w.length > 5) w = w.slice(0, -2);
  else if (w.endsWith("er") && w.length > 5) w = w.slice(0, -2);
  // "running" → "runn", "stopped" → "stopp": one consonant is the stem.
  if (w.length > 3 && /([b-df-hj-np-tv-z])\1$/.test(w) && !/(?:ll|ss|zz)$/.test(w)) w = w.slice(0, -1);
  return w;
}

/**
 * Words that name the same idea on a software task board. Each group is one
 * concept; a word may sit in more than one group. Kept to concepts a reader
 * would expect to find each other — "auth" finds "login", "slow" finds
 * "latency" — not a thesaurus.
 */
export const CONCEPTS: readonly (readonly string[])[] = [
  ["auth", "authentication", "authenticate", "login", "logon", "signin", "sign", "logout", "signout", "sso", "oauth", "session", "credential", "password", "passkey"],
  ["authorization", "authorize", "permission", "access", "acl", "role", "rbac", "grant", "privilege", "scope"],
  ["bug", "defect", "issue", "error", "fault", "glitch", "problem", "broken", "regression", "fix", "wrong"],
  ["crash", "panic", "segfault", "abort", "exception", "fatal", "hang", "freeze", "deadlock"],
  ["slow", "perf", "performance", "latency", "lag", "sluggish", "speed", "fast", "faster", "optimize", "optimise", "throughput", "bottleneck"],
  ["memory", "leak", "oom", "ram", "heap", "allocation"],
  ["ui", "ux", "interface", "frontend", "layout", "style", "css", "design", "visual", "screen", "view"],
  ["button", "control", "widget", "component"],
  ["api", "endpoint", "route", "rest", "graphql", "rpc", "ipc", "request", "handler"],
  ["db", "database", "sql", "sqlite", "postgres", "mysql", "query", "schema", "migration", "table", "store", "storage"],
  ["test", "tests", "testing", "spec", "unit", "e2e", "integration", "coverage", "flaky", "assert"],
  ["doc", "docs", "documentation", "readme", "guide", "manual", "wiki", "changelog"],
  ["deploy", "deployment", "release", "ship", "rollout", "publish", "launch"],
  ["ci", "pipeline", "build", "workflow", "action", "actions", "job", "compile"],
  ["config", "configuration", "setting", "settings", "preference", "option", "env", "environment"],
  ["security", "vulnerability", "vuln", "cve", "exploit", "xss", "csrf", "injection", "secret", "token", "encrypt", "encryption"],
  ["refactor", "cleanup", "restructure", "rewrite", "simplify", "tidy", "debt"],
  ["feature", "enhancement", "improvement", "request", "idea", "proposal"],
  ["remove", "delete", "drop", "deprecate", "purge"],
  ["add", "create", "new", "introduce", "implement", "support"],
  ["update", "upgrade", "bump", "change", "modify", "edit"],
  ["dependency", "dependencies", "dep", "deps", "package", "library", "lib", "crate", "module", "vendor"],
  ["log", "logging", "logs", "trace", "tracing", "telemetry", "metric", "metrics", "monitor", "monitoring", "observability"],
  ["notification", "notify", "alert", "toast", "email", "message", "inbox"],
  ["search", "find", "filter", "lookup", "query", "index"],
  ["file", "files", "path", "directory", "folder", "fs", "filesystem"],
  ["git", "commit", "branch", "merge", "rebase", "diff", "repo", "repository", "checkout", "worktree"],
  ["pr", "pull", "review", "reviewer", "approval", "approve"],
  ["mobile", "ios", "android", "phone", "tablet"],
  ["mac", "macos", "osx", "darwin", "apple"],
  ["windows", "win", "win32"],
  ["linux", "ubuntu", "debian", "fedora"],
  ["network", "http", "https", "socket", "websocket", "proxy", "dns", "connection", "offline", "timeout"],
  ["cache", "caching", "memoize", "stale", "invalidate"],
  ["sync", "synchronize", "replicate", "mirror", "reconcile"],
  ["import", "export", "upload", "download", "migrate"],
  ["keyboard", "shortcut", "hotkey", "keybinding", "a11y", "accessibility", "screenreader", "aria", "focus"],
  ["dark", "theme", "color", "colour", "contrast", "palette"],
  ["i18n", "l10n", "locale", "translation", "translate", "language", "localization"],
  ["agent", "ai", "llm", "model", "claude", "assistant", "bot", "codex"],
  ["terminal", "shell", "console", "cli", "command", "tty", "pty"],
  ["task", "todo", "ticket", "card", "item", "work"],
  ["urgent", "critical", "blocker", "asap", "emergency", "p0", "hotfix"],
  ["docker", "container", "kubernetes", "k8s", "image", "pod"],
  ["payment", "billing", "invoice", "checkout", "subscription", "stripe"],
  ["user", "account", "profile", "member", "customer"],
  ["startup", "boot", "launch", "init", "initialize", "load", "loading"],
  ["upgrade", "migration", "compat", "compatibility", "backward", "legacy"],
];

/** Each stemmed word → the stems of every word it shares a concept with. */
const SYNONYMS: ReadonlyMap<string, ReadonlySet<string>> = (() => {
  const map = new Map<string, Set<string>>();
  for (const group of CONCEPTS) {
    const stems = [...new Set(group.flatMap((word) => words(word).map(stem)))];
    for (const s of stems) {
      const set = map.get(s) ?? new Set<string>();
      for (const other of stems) if (other !== s) set.add(other);
      map.set(s, set);
    }
  }
  return map;
})();

const NONE: ReadonlySet<string> = new Set();

/** The stems that name the same concept as `stemmed` (never itself). */
export function synonymsOf(stemmed: string): ReadonlySet<string> {
  return SYNONYMS.get(stemmed) ?? NONE;
}

/** How many edits a word of this length may carry and still be the word meant. */
export function typoBudget(length: number): number {
  return length >= 9 ? 2 : length >= 5 ? 1 : 0;
}

/**
 * Optimal string alignment distance (Damerau: a swap is one edit), giving up
 * past `max` — the answer is then `max + 1`, never the true distance.
 */
export function editDistance(a: string, b: string, max = 2): number {
  if (a === b) return 0;
  if (Math.abs(a.length - b.length) > max) return max + 1;
  const rows = b.length + 1;
  let prev2 = new Array<number>(rows).fill(0);
  let prev = Array.from({ length: rows }, (_, j) => j);
  let cur = new Array<number>(rows).fill(0);
  for (let i = 1; i <= a.length; i++) {
    cur[0] = i;
    let best = cur[0];
    for (let j = 1; j < rows; j++) {
      const cost = a[i - 1] === b[j - 1] ? 0 : 1;
      let value = Math.min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + cost);
      if (i > 1 && j > 1 && a[i - 1] === b[j - 2] && a[i - 2] === b[j - 1]) value = Math.min(value, prev2[j - 2] + 1);
      cur[j] = value;
      if (value < best) best = value;
    }
    if (best > max) return max + 1;
    [prev2, prev, cur] = [prev, cur, prev2];
  }
  return Math.min(prev[rows - 1], max + 1);
}
