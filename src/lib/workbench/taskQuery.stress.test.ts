import { describe, expect, it } from "vitest";
import type { TaskCard, TaskStatus } from "./client";
import { STATUSES } from "./vocabulary";
import { dueState } from "./taskOrganize";
import {
  cardMatchesQuery,
  MAX_QUERY_TERMS,
  MAX_QUERY_TEXT,
  parseTaskQuery,
  queryHas,
  removeQueryTerm,
  toggleQueryTerm,
  type QueryContext,
} from "./taskQuery";

/** Deterministic, so a failure names a seed that reproduces it. */
function rng(seed: number) {
  let s = seed >>> 0;
  return () => {
    s = (s + 0x6d2b79f5) >>> 0;
    let t = s;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const KINDS = ["bug", "feature", "Bug", "research", ""];
const OWNERS = ["Pat", "ann", null, "  "];
const LABELS = ["ui", "ci", "Needs Review", " "];
const SEVERITIES = ["high", "Low", null, ""];
const REPOS = ["r1", "r2", "r3"];
const NAMES: Record<string, { name: string; identity_key: string }> = {
  r1: { name: "GitPulse", identity_key: "local:/Users/me/code/GitPulse/.git" },
  r2: { name: "Manvi", identity_key: "github.com/acme/manvi" },
  r3: { name: "Tools", identity_key: "local:/Users/me/code/tools/.git" },
};
const ctx: QueryContext = { repository: (id) => NAMES[id], nowSec: 1_000 };

function randomCard(r: () => number, i: number): TaskCard {
  const pick = <T,>(xs: readonly T[]) => xs[Math.floor(r() * xs.length)];
  return {
    id: `t${i}`, revision: 1, updated_at: 1, title: pick(["Crash on save", "Slow start", "flaky test", ""]),
    kind: pick(KINDS), status: pick(STATUSES), priority: Math.floor(r() * 4), severity: pick(SEVERITIES),
    owner: pick(OWNERS), due_at: pick([null, 0, 500, 1_500, 1_000 + 30 * 86_400]),
    labels: LABELS.filter(() => r() < 0.4), repository_ids: REPOS.filter(() => r() < 0.5),
    primary_repository_id: "r1", home_workspace_id: null, position: i, archived: false, completed_at: null,
  };
}

const FILTERS = [
  "is:bug", "type:feature", "is:bugs", "priority:urgent", "p:p2", "status:ready", "status:open", "status:in-progress",
  "owner:pat", "owner:none", "label:ui", 'label:"needs review"', "label:none", "severity:high", "severity:none",
  "repo:pulse", "repo:r3", "repo:acme", "repo:git", "repo:code", "repo:me/code/tools", "repo:co", "is:open", "is:closed", "is:unassigned", "is:overdue", "due:overdue", "due:soon", "due:none", "due:any", "due:later",
];

function randomQuery(r: () => number): string {
  const parts: string[] = [];
  const n = Math.floor(r() * 6);
  for (let i = 0; i < n; i++) {
    const roll = r();
    if (roll < 0.6) parts.push(`${r() < 0.25 ? "-" : ""}${FILTERS[Math.floor(r() * FILTERS.length)]}`);
    else if (roll < 0.75) parts.push(["crash", "slow", "-flaky", "save"][Math.floor(r() * 4)]);
    else if (roll < 0.85) parts.push(["priority:huge", "status:nope", "due:never", "is:", "-"][Math.floor(r() * 5)]);
    else parts.push(["https://x.y", "10:30", "\"quoted words\"", "!!", "été"][Math.floor(r() * 5)]);
  }
  return parts.join(" ".repeat(1 + Math.floor(r() * 3)));
}

/**
 * The reference reading, written out longhand and independently of the
 * module: one predicate per filter string, combined per the documented rule.
 */
function oracle(card: TaskCard, query: string): boolean {
  const owner = (card.owner ?? "").trim().toLowerCase();
  const labels = card.labels.map((l) => l.trim().toLowerCase()).filter(Boolean);
  const sev = (card.severity ?? "").trim().toLowerCase();
  const kind = card.kind.trim().toLowerCase();
  const due = dueState(card.due_at, ctx.nowSec);
  // Longhand: name substring; a whole folder/owner/host; with a slash, any stretch.
  const named = (id: string, needle: string) => {
    const repo = NAMES[id];
    if (!repo) return false;
    if (repo.name.toLowerCase().includes(needle)) return true;
    const identity = repo.identity_key.toLowerCase();
    if (needle.includes("/")) return identity.includes(needle);
    const parts: string[] = [];
    let part = "";
    for (const ch of identity) {
      if (ch === "/" || ch === ":" || ch === "@" || ch === "\\") { parts.push(part); part = ""; } else part += ch;
    }
    parts.push(part);
    return parts.some((p) => p === needle || (p.endsWith(".git") && p.slice(0, -4) === needle));
  };
  const repoHas = (needle: string) => card.repository_ids.some((id) => id === needle || named(id, needle));
  const table: Record<string, [string, () => boolean]> = {
    "is:bug": ["kind", () => kind === "bug"], "type:feature": ["kind", () => kind === "feature"], "is:bugs": ["kind", () => kind === "bug" || kind === "bugs"],
    "priority:urgent": ["priority", () => card.priority === 0], "p:p2": ["priority", () => card.priority === 2],
    "status:ready": ["status", () => card.status === "ready"], "status:open": ["status", () => card.status !== "done"],
    "status:in-progress": ["status", () => card.status === ("in_progress" satisfies TaskStatus)],
    "owner:pat": ["owner", () => owner === "pat"], "owner:none": ["owner", () => owner === ""],
    "label:ui": ["label", () => labels.includes("ui")], 'label:"needs review"': ["label", () => labels.includes("needs review")], "label:none": ["label", () => labels.length === 0],
    "severity:high": ["severity", () => sev === "high"], "severity:none": ["severity", () => sev === ""],
    "repo:pulse": ["repo", () => repoHas("pulse")], "repo:r3": ["repo", () => repoHas("r3")], "repo:acme": ["repo", () => repoHas("acme")],
    "repo:git": ["repo", () => repoHas("git")], "repo:code": ["repo", () => repoHas("code")], "repo:me/code/tools": ["repo", () => repoHas("me/code/tools")], "repo:co": ["repo", () => repoHas("co")],
    "is:open": ["status", () => card.status !== "done"], "is:closed": ["status", () => card.status === "done"],
    "is:unassigned": ["owner", () => owner === ""], "is:overdue": ["due", () => due === "overdue"],
    "due:overdue": ["due", () => due === "overdue"], "due:soon": ["due", () => due === "soon"], "due:none": ["due", () => due === "none"],
    "due:any": ["due", () => due !== "none"], "due:later": ["due", () => due === "later"],
  };
  const groups = new Map<string, boolean>();
  // A run of non-space, keeping a quoted value with its key (`label:"needs review"`).
  const tokens = query.match(/(?:[^\s"]+|"[^"]*")+/g) ?? [];
  let applied = 0;
  for (const token of tokens) {
    const negated = token.length > 1 && token.startsWith("-");
    const body = negated ? token.slice(1) : token;
    const entry = table[body];
    if (entry) {
      if (applied++ >= MAX_QUERY_TERMS) continue;
      const holds = entry[1]();
      if (negated) { if (holds) return false; continue; }
      groups.set(entry[0], (groups.get(entry[0]) ?? false) || holds);
    } else if (negated && body === "flaky") {
      if (applied++ >= MAX_QUERY_TERMS) continue;
      if (card.title.toLowerCase().includes("flaky")) return false;
    }
  }
  return [...groups.values()].every(Boolean);
}

describe("taskQuery under random input", () => {
  it("agrees with the longhand reading on 20,000 card/query pairs", () => {
    const r = rng(0x5eed);
    const cards = Array.from({ length: 200 }, (_, i) => randomCard(r, i));
    let checked = 0, matched = 0;
    for (let q = 0; q < 100; q++) {
      const query = randomQuery(r);
      const parsed = parseTaskQuery(query);
      for (const card of cards) {
        const got = cardMatchesQuery(card, parsed, ctx);
        if (got !== oracle(card, query)) throw new Error(`disagree on ${JSON.stringify(query)} for ${JSON.stringify(card)}`);
        checked++; if (got) matched++;
      }
    }
    expect(checked).toBe(20_000);
    // Neither all nor nothing: the corpus exercises both outcomes.
    expect(matched).toBeGreaterThan(1_000);
    expect(matched).toBeLessThan(19_000);
  });

  it("never throws and stays bounded on arbitrary bytes", () => {
    const r = rng(42);
    const alphabet = 'abc:-" \t\n\u0000​😀is:priority:repo:label:due:é\\';
    for (let i = 0; i < 5_000; i++) {
      const len = Math.floor(r() * 900);
      let input = "";
      for (let j = 0; j < len; j++) input += alphabet[Math.floor(r() * alphabet.length)];
      const parsed = parseTaskQuery(input);
      expect(parsed.text.length).toBeLessThanOrEqual(MAX_QUERY_TEXT);
      expect(parsed.terms.length).toBeLessThanOrEqual(MAX_QUERY_TERMS);
      for (const term of parsed.terms) {
        const source = input.slice(0, MAX_QUERY_TEXT);
        expect(source.slice(term.start, term.end)).toBe(term.raw);
      }
    }
  });

  it("removing each chip removes exactly that filter and nothing else", () => {
    const r = rng(7);
    for (let i = 0; i < 2_000; i++) {
      const query = randomQuery(r);
      const before = parseTaskQuery(query);
      for (const term of before.terms) {
        const after = parseTaskQuery(removeQueryTerm(query, term));
        const key = (t: { key: string; value: string; negated: boolean }) => `${t.negated ? "-" : ""}${t.key}:${t.value}`;
        const expected = before.terms.filter((other) => other !== term).map(key).sort();
        expect(after.terms.map(key).sort()).toEqual(expected);
        expect(after.text).toBe(before.text);
      }
    }
  });

  it("toggling a quick filter twice gives back the same filters and text", () => {
    const r = rng(99);
    for (let i = 0; i < 2_000; i++) {
      const query = randomQuery(r);
      const filter = FILTERS[Math.floor(r() * FILTERS.length)];
      const once = toggleQueryTerm(query, filter);
      const twice = toggleQueryTerm(once, filter);
      const had = queryHas(parseTaskQuery(query), filter);
      expect(queryHas(parseTaskQuery(once), filter)).toBe(!had);
      if (!had) {
        // Added then removed: the reader's search comes back word for word, spacing aside.
        expect(twice).toBe(query.replace(/\s+/g, " ").trim());
      } else {
        expect(queryHas(parseTaskQuery(twice), filter)).toBe(true);
      }
    }
  });

  it("filters 5,000 cards with 32 filters well inside a frame", () => {
    const r = rng(1);
    const cards = Array.from({ length: 5_000 }, (_, i) => randomCard(r, i));
    const parsed = parseTaskQuery(FILTERS.concat(FILTERS).slice(0, MAX_QUERY_TERMS).map((f, i) => (i % 3 ? f : `-${f}`)).join(" "));
    expect(parsed.terms).toHaveLength(MAX_QUERY_TERMS);
    let best = Infinity;
    for (let run = 0; run < 5; run++) {
      const start = performance.now();
      cards.filter((c) => cardMatchesQuery(c, parsed, ctx));
      best = Math.min(best, performance.now() - start);
    }
    // Generous: a frame is 16 ms; CI under load is slow. This guards against
    // an accidental quadratic, not a micro-benchmark.
    expect(best).toBeLessThan(50);
  });
});
