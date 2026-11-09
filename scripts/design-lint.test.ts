import { execFileSync, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  DEFAULT_BASELINE_FILE,
  RULES,
  buildBaseline,
  compareToBaseline,
  countViolations,
  lintRepo,
  lintSource,
  main,
  readBaseline,
  type Violation,
} from "./design-lint.mjs";

const FIXTURES = fileURLToPath(new URL("./fixtures/design-lint/", import.meta.url));

function fixture(name: string): string {
  return readFileSync(path.join(FIXTURES, name), "utf8");
}

/** The fixture is linted under a path of the kind the rule scans. */
function hits(rule: number, name: string, as?: string): Violation[] {
  const target = as ?? (name.endsWith(".ts.txt") ? "src/lib/fixture.ts" : name.endsWith(".css") ? "src/app.css" : `src/lib/${name}`);
  return lintSource(target, fixture(name)).filter((v) => v.rule === rule);
}

const pad = (n: number) => String(n).padStart(2, "0");

/** rule -> [positive count, svelte negative file exists] */
const SVELTE_RULES: Record<number, number> = { 1: 4, 2: 5, 3: 7, 4: 6, 5: 3, 6: 4, 7: 5, 8: 4, 9: 6, 10: 6, 11: 4, 12: 5, 13: 5, 14: 10 };

describe("design-lint rules: one positive and one negative fixture each", () => {
  for (const [rule, expected] of Object.entries(SVELTE_RULES)) {
    const id = Number(rule);
    it(`rule ${id} flags its positive fixture`, () => {
      const found = hits(id, `rule-${pad(id)}.positive.svelte`);
      expect(found.map((v) => `${v.line}:${v.message}`)).toHaveLength(expected);
    });
    it(`rule ${id} stays silent on its negative fixture`, () => {
      const found = hits(id, `rule-${pad(id)}.negative.svelte`);
      expect(found.map((v) => `${v.line}:${v.message}`)).toEqual([]);
    });
  }

  it("rule 15 flags an animated class missing from either reduced-motion list, and a list that disagrees with the other", () => {
    const found = hits(15, "rule-15.positive.css");
    const messages = found.map((v) => v.message).sort();
    expect(messages).toEqual(
      [
        ".gp-fancy animates but is not in the @media (prefers-reduced-motion: reduce) list",
        '.gp-fancy animates but is not in the html[data-motion="reduced"] list',
        ".unlisted animates but is not in the @media (prefers-reduced-motion: reduce) list",
        '.unlisted animates but is not in the html[data-motion="reduced"] list',
        ".only-media is in the prefers-reduced-motion list but not the data-motion list",
        ".only-attr is in the data-motion list but not the prefers-reduced-motion list",
      ].sort(),
    );
  });

  it("rule 15 accepts listed classes, gated no-preference rules, comments, keyframes and class*= patterns", () => {
    expect(hits(15, "rule-15.negative.css")).toEqual([]);
  });

  it("rule 15 reads only src/app.css", () => {
    expect(lintSource("src/other.css", fixture("rule-15.positive.css")).filter((v) => v.rule === 15)).toEqual([]);
  });

  it("rule 3 also scans TypeScript class strings, and ignores comments and plain words there", () => {
    expect(hits(3, "rule-03.positive.ts.txt").map((v) => v.message.split(":")[0])).toEqual(["text-rose-400", "bg-rose-500/20"]);
    expect(hits(3, "rule-03.negative.ts.txt")).toEqual([]);
  });

  it("rule 10 scans TypeScript for duration literals only, ignoring comments, tokens and strings", () => {
    expect(hits(10, "rule-10.positive.ts.txt")).toHaveLength(2);
    expect(hits(10, "rule-10.negative.ts.txt")).toEqual([]);
  });

  it("rule 10 leaves its owners alone", () => {
    expect(lintSource("src/lib/motion/tokens.ts", fixture("rule-10.positive.ts.txt")).filter((v) => v.rule === 10)).toEqual([]);
    expect(lintSource("src/lib/ui/transitions.ts", fixture("rule-10.positive.ts.txt")).filter((v) => v.rule === 10)).toEqual([]);
  });

  it("rule 3 reads the copy vocabulary like any other file: it holds no palette class", () => {
    const real = readFileSync(fileURLToPath(new URL("../src/lib/ui/copy/vocabulary.ts", import.meta.url)), "utf8");
    expect(lintSource("src/lib/ui/copy/vocabulary.ts", real).filter((v) => v.rule === 3)).toEqual([]);
    expect(lintSource("src/lib/ui/copy/vocabulary.ts", 'export const n = "border-red-500/60!";\n').filter((v) => v.rule === 3)).toHaveLength(1);
  });

  it("TypeScript files run no rule but 3 and 10", () => {
    const ts = lintSource("src/lib/x.ts", 'export const c = "text-[10px] border-t z-50 rounded-3xl text-amber-400 uppercase tracking-wide";\n');
    expect([...new Set(ts.map((v) => v.rule))]).toEqual([3]);
  });
});

describe("design-lint precision: the tricky negatives, one by one", () => {
  const svelte = (markup: string, rule: number) => lintSource("src/lib/x.svelte", markup).filter((v) => v.rule === rule);

  it("a spread is code, never an ellipsis in text", () => {
    expect(svelte("<script>\n  const a = [...items];\n  f(...args);\n  const o = { ...x };\n</script>\n<div {...rest}></div>\n", 13)).toEqual([]);
  });

  it("an ASCII ellipsis in a string is a hit and the real character is not", () => {
    expect(svelte('<p>{busy ? "Saving..." : "Saved"}</p>', 13)).toHaveLength(1);
    expect(svelte('<p>{busy ? "Saving…" : "Saved"}</p>', 13)).toEqual([]);
  });

  it("hex in an HTML, JS or CSS comment is not a hit", () => {
    expect(svelte("<!-- #ff0000 -->\n<script>\n  // #ff0000\n  /* #00ff00 */\n</script>\n<style>\n  /* #0000ff */\n</style>\n", 4)).toEqual([]);
  });

  it("text-[var(--x)] and other non-px arbitrary values are not type-scale hits", () => {
    expect(svelte('<p class="text-[var(--x)] text-[length:var(--y)] text-[2rem] text-[color:var(--z)]"></p>', 1)).toEqual([]);
  });

  it("allow-listed border utilities exempt rule 7, a variant-prefixed hairline does not", () => {
    for (const cls of ["gp-menu-sep", "gp-separator", "gp-card", "gp-field", "gp-field-multi", "gp-btn", "gp-chip", "gp-floor", "gp-select"]) {
      expect(svelte(`<div class="${cls} border-t"></div>`, 7)).toEqual([]);
    }
    expect(svelte('<div class="md:border-t"></div>', 7)).toHaveLength(1);
  });

  it("transition-colors is allowed while transition-all is not", () => {
    expect(svelte('<a class="transition-colors"></a>', 9)).toEqual([]);
    expect(svelte('<a class="transition-all"></a>', 9)).toHaveLength(1);
  });

  it("variants, the important marker and an opacity suffix are peeled before matching", () => {
    expect(svelte('<a class="md:hover:!text-[10px] dark:bg-rose-500/20"></a>', 1)).toHaveLength(1);
    expect(svelte('<a class="md:hover:!text-[10px] dark:bg-rose-500/20"></a>', 3)).toHaveLength(1);
  });

  it("a block close tag is not read as the start of a regex literal", () => {
    const src = '{#if a}<b>x</b>{/if}<div class="text-[10px]">y</div>\n<p class="text-[11px]">z</p>';
    expect(svelte(src, 1)).toHaveLength(2);
  });

  it("a regex literal holding a quote does not swallow the rest of the script", () => {
    const src = '<script>\n  const re = /"[^"]*"/g;\n  const c = "text-[10px]";\n</script>\n';
    expect(svelte(src, 1)).toHaveLength(1);
  });

  it("class directives and *Class props are class sources", () => {
    expect(svelte('<Card iconClass="text-[10px]" class:text-[9px]={x} />', 1)).toHaveLength(2);
  });

  it("scoped styles: .gp-* owners, :not() contents and keyframes are not widget dialects", () => {
    expect(svelte("<style>\n  :global(.gp-seg-btn) { a: b }\n  .x:not(.chip) { a: b }\n  @keyframes link { from { a: b } }\n</style>", 12)).toEqual([]);
    expect(svelte("<style>\n  .chip { a: b }\n</style>", 12)).toHaveLength(1);
  });

  it("hex needs a color position outside CSS: a whole quoted value or a [#hex] arbitrary value", () => {
    expect(svelte('<p title="Fixes #1234">x</p><button data-go={"#panel"}>y</button>', 4)).toEqual([]);
    expect(svelte("<p class=\"bg-[#abc]/50\"></p><i style={`color: ${c}`}></i>{x ? '#fff' : '#000'}", 4)).toHaveLength(3);
  });
});

describe("design-lint registry", () => {
  it("has one entry per rule 1-15, rule 14 being the single banned-terms entry", () => {
    expect(RULES.map((r) => r.id)).toEqual([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]);
    expect(new Set(RULES.map((r) => r.name)).size).toBe(RULES.length);
    for (const r of RULES) expect(r.summary).not.toBe("");
    expect(RULES.filter((r) => r.id === 14).map((r) => r.name)).toEqual(["banned-ui-terms"]);
  });
});

describe("design-lint rule 14: banned UI terms", () => {
  const rule14 = (src: string, file = "src/lib/x.svelte") => lintSource(file, src).filter((v) => v.rule === 14);

  it("reads exactly the vocabulary's BANNED_UI_TERMS, and nothing from the retired-verb or voice lists", () => {
    const found = rule14("<p>It will reload and you must click Click here! Fetch it successfully</p>");
    expect(found.map((v) => v.message)).toEqual([expect.stringContaining("successfully")]);
    // RETIRED_VERB_TERMS / VOICE_TERMS words ("reload", "!") are not rule 14.
    expect(rule14("<p>Reload now!</p>")).toEqual([]);
  });

  it("names the hit, the listed term and the replacement, at the position of the match", () => {
    const [hit] = rule14("<p>\n  Last attempt</p>");
    expect(hit).toMatchObject({ rule: 14, line: 2, col: 8 });
    expect(hit.message).toBe('"attempt" (attempt): say "run"');
  });

  it("reports every occurrence in a text node, and each entry that matches the same words", () => {
    expect(rule14("<p>An attempt, then another attempt</p>")).toHaveLength(2);
    expect(rule14("<p>The repo map</p>").map((v) => v.message.match(/\(([^)]+)\)/)?.[1])).toEqual(["repo map", "repo"]);
  });

  it("is case sensitive where the vocabulary is (MANVI yes, Manvi no) and not where it is not", () => {
    expect(rule14("<p>MANVI policy</p>")).toHaveLength(1);
    expect(rule14("<p>Manvi policy</p>")).toEqual([]);
    expect(rule14("<p>Options</p><p>options</p>")).toHaveLength(1);
    expect(rule14("<p>ATTEMPT</p>")).toHaveLength(1);
  });

  it("does not see code: bare identifiers and property access, a script, an import path or a comment", () => {
    const src = [
      '<script lang="ts">',
      '  import { x } from "$lib/projects/attempt-store";',
      '  const label = "Retry attempt";',
      "</script>",
      "<!-- the attempt lane -->",
      "<p>{attempt} {run.attempt.lane} {project} {harness?.repo}</p>",
    ].join("\n");
    expect(rule14(src)).toEqual([]);
  });

  it("reads the string literals of a mustache in text or in a display attribute", () => {
    expect(rule14('<p>{cond ? "Retry attempt" : "Done"}</p>')).toHaveLength(1);
    expect(rule14('<p>{cond ? `an attempt on ${x}` : "none"}</p>')).toHaveLength(1);
    expect(rule14('<a title={x ? "Open repo" : "Close"}>y</a>')).toHaveLength(1);
    expect(rule14('<Panel heading={x || "No project"} />')).toHaveLength(1);
    expect(rule14('<a class={x ? "attempt" : ""} href={"/repo"} id={"lane"} data-testid={"attempt"}>y</a>')).toEqual([]);
  });

  it("skips the mustache strings that are code: comparisons, call arguments, block tags, handlers", () => {
    expect(rule14('<p>{kind === "attempt" ? "Done" : "Open"}</p>')).toEqual([]);
    expect(rule14('<p>{"attempt" == kind ? "Done" : "Open"} {kind !== "lane"}</p>')).toEqual([]);
    expect(rule14('<p>{invoke("devmap_status")} {f(a, "attempt")}</p>')).toEqual([]);
    expect(rule14('{#if kind === "lane"}<p>x</p>{:else if kind === "attempt"}<p>y</p>{/if}')).toEqual([]);
    expect(rule14('<Row onTools={() => open("devmap")} onclick={() => go("project")} />')).toEqual([]);
    // A grouping paren is not a call, and copy next to a comparison is still copy.
    expect(rule14('<p>{(busy ? "Retry attempt" : "Done")}</p>')).toHaveLength(1);
    expect(rule14('<p>{kind === "x" ? "Retry attempt" : "Done"}</p>')).toHaveLength(1);
  });

  it("does not read data-testid or other data-* hooks, on elements or components, but does read data-tip", () => {
    expect(rule14('<button data-testid="attempt-retry"></button><Row data-testid="project lane" data-x="harness" />')).toEqual([]);
    expect(rule14('<span data-tip="Open the project"></span>')).toHaveLength(1);
    expect(rule14('<span data-tooltip="Open the project"></span>')).toHaveLength(1);
  });

  it("reads the display attributes of an element and any prop of a component, not class, href or id", () => {
    expect(rule14('<a title="attempt" aria-label="lane" placeholder="project" alt="harness" label="MANVI"></a>')).toHaveLength(5);
    expect(rule14('<Panel heading="attempt" caption="lane"></Panel>')).toHaveLength(2);
    expect(rule14('<a class="attempt lane" href="/project/repo" id="harness"></a>')).toEqual([]);
  });

  it("is a .svelte rule: TypeScript and CSS files never run it", () => {
    expect(lintSource("src/lib/x.ts", 'export const t = "Retry attempt";\n').filter((v) => v.rule === 14)).toEqual([]);
    expect(lintSource("src/app.css", ".attempt { content: 'attempt'; }\n").filter((v) => v.rule === 14)).toEqual([]);
  });

  it("the linter and the vocabulary agree: every listed term is a hit in text, via the shared list", async () => {
    const { BANNED_UI_TERMS } = await import("../src/lib/ui/copy/vocabulary");
    const samples: Record<string, string> = {
      "code graph": "code graph", "repo map": "repo map", codeintel: "codeintel", devmap: "devmap", MANVI: "MANVI",
      harness: "harness", sidecar: "sidecar", posture: "posture", attempt: "attempt", lane: "lane",
      "linked checkout": "linked checkout", ticket: "ticket", repo: "repo", project: "project", Preferences: "Preferences",
      Options: "Options", "min rung": "min rung", rung: "rung", "ledger record": "ledger record", "generation N": "generation 12",
      is_fresh: "is_fresh", "snake_case identifier": "some_name", successfully: "successfully", Oops: "Oops",
    };
    for (const t of BANNED_UI_TERMS) {
      expect(samples[t.term], `a sample for ${t.term}`).toBeDefined();
      const messages = rule14(`<p>${samples[t.term]}</p>`).map((v) => v.message);
      expect(messages.some((m) => m.includes(`(${t.term})`)), t.term).toBe(true);
    }
  });
});

describe("design-lint ratchet", () => {
  const v = (rule: number, file: string, n: number): Violation[] =>
    Array.from({ length: n }, (_, i) => ({ rule, file, line: i + 1, col: 1, message: "m" }));

  it("builds a baseline with per-rule, per-file counts, totals and the commit", () => {
    const b = buildBaseline([...v(1, "src/a.svelte", 2), ...v(1, "src/b.svelte", 1), ...v(3, "src/a.svelte", 4)], "abc123");
    expect(b.commit).toBe("abc123");
    expect(b.rules["1"]).toMatchObject({ total: 3, files: { "src/a.svelte": 2, "src/b.svelte": 1 } });
    expect(b.rules["3"].total).toBe(4);
    expect(b.rules["2"]).toMatchObject({ total: 0, files: {} });
    expect(b.retake).toContain("BASE_SHA");
  });

  it("flags an increase, a new file with violations and a rule absent from the baseline", () => {
    const base = buildBaseline(v(1, "src/a.svelte", 2), "c");
    const cmp = compareToBaseline(countViolations([...v(1, "src/a.svelte", 3), ...v(1, "src/new.svelte", 1), ...v(5, "src/a.svelte", 1)]), base);
    expect(cmp.increases).toEqual([
      { rule: 1, file: "src/a.svelte", before: 2, after: 3, isNew: false },
      { rule: 1, file: "src/new.svelte", before: 0, after: 1, isNew: true },
      { rule: 5, file: "src/a.svelte", before: 0, after: 1, isNew: true },
    ]);
  });

  it("reports a decrease as progress and does not count it as an increase", () => {
    const base = buildBaseline([...v(1, "src/a.svelte", 5), ...v(2, "src/b.svelte", 1)], "c");
    const cmp = compareToBaseline(countViolations(v(1, "src/a.svelte", 2)), base);
    expect(cmp.increases).toEqual([]);
    expect(cmp.decreases).toEqual([
      { rule: 1, file: "src/a.svelte", before: 5, after: 2 },
      { rule: 2, file: "src/b.svelte", before: 1, after: 0 },
    ]);
  });

  it("holds when the counts are equal", () => {
    const base = buildBaseline(v(1, "src/a.svelte", 2), "c");
    expect(compareToBaseline(countViolations(v(1, "src/a.svelte", 2)), base)).toEqual({ increases: [], decreases: [] });
  });
});

describe("design-lint CLI against a scratch repository", () => {
  let dir = "";
  let logs: string[] = [];

  const git = (...args: string[]) => execFileSync("git", ["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false", ...args], { cwd: dir, encoding: "utf8" }).trim();
  const write = (rel: string, body: string) => {
    mkdirSync(path.dirname(path.join(dir, rel)), { recursive: true });
    writeFileSync(path.join(dir, rel), body);
  };
  const run = (...args: string[]) => {
    logs = [];
    const code = main(["--root", dir, ...args]);
    return { code, out: logs.join("\n") };
  };

  beforeEach(() => {
    dir = mkdtempSync(path.join(tmpdir(), "design-lint-"));
    logs = [];
    vi.spyOn(console, "log").mockImplementation((...a: unknown[]) => void logs.push(a.join(" ")));
    vi.spyOn(console, "error").mockImplementation((...a: unknown[]) => void logs.push(a.join(" ")));
    git("init", "-q");
    write("src/a.svelte", '<p class="text-[10px] text-[11px]">a</p>\n');
    git("add", "-A");
    git("commit", "-q", "-m", "seed");
  });

  afterEach(() => {
    vi.restoreAllMocks();
    rmSync(dir, { recursive: true, force: true });
  });

  it("cannot run without a baseline, and says so with exit 2 rather than passing", () => {
    const r = run();
    expect(r.code).toBe(2);
    expect(r.out).toContain("no baseline");
  });

  it("writes the baseline with the HEAD sha and then passes against it", () => {
    expect(run("--baseline").code).toBe(0);
    const written = JSON.parse(readFileSync(path.join(dir, "scripts", "design-lint.baseline.json"), "utf8"));
    expect(written.commit).toBe(git("rev-parse", "HEAD"));
    expect(written.rules["1"].files).toEqual({ "src/a.svelte": 2 });
    expect(run().code).toBe(0);
  });

  it("refuses to take a baseline over uncommitted changes under src/", () => {
    write("src/a.svelte", '<p class="text-[10px]">a</p>\n');
    const r = run("--baseline");
    expect(r.code).toBe(2);
    expect(r.out).toContain("uncommitted");
    expect(existsSync(path.join(dir, "scripts", "design-lint.baseline.json"))).toBe(false);
  });

  it("fails when a count rises, names the line, and passes again once it is fixed", () => {
    run("--baseline");
    write("src/a.svelte", '<p class="text-[10px] text-[11px]">a</p>\n<p class="text-[12px]">b</p>\n');
    const bad = run();
    expect(bad.code).toBe(1);
    expect(bad.out).toContain("rule 1  src/a.svelte: 2 -> 3");
    expect(bad.out).toContain("src/a.svelte:2:");
    write("src/a.svelte", '<p class="text-[10px] text-[11px]">a</p>\n');
    expect(run().code).toBe(0);
  });

  it("fails on a new file with a violation and passes on a new clean file", () => {
    run("--baseline");
    write("src/clean.svelte", '<p class="text-xs">ok</p>\n');
    expect(run().code).toBe(0);
    write("src/new.svelte", '<p class="z-50">x</p>\n');
    const bad = run();
    expect(bad.code).toBe(1);
    expect(bad.out).toContain("src/new.svelte: 0 -> 1 (new)");
  });

  it("prints a decrease as progress and still exits 0", () => {
    run("--baseline");
    write("src/a.svelte", '<p class="text-[10px]">a</p>\n');
    const r = run();
    expect(r.code).toBe(0);
    expect(r.out).toContain("progress: 1 violation fixed");
  });

  it("--json carries the verdict, totals and the violations over baseline", () => {
    run("--baseline");
    write("src/a.svelte", '<p class="text-[10px] text-[11px] text-[12px]">a</p>\n');
    const r = run("--json");
    const parsed = JSON.parse(r.out);
    expect(r.code).toBe(1);
    expect(parsed.ok).toBe(false);
    expect(parsed.totals["1"]).toBe(3);
    expect(parsed.baselineTotals["1"]).toBe(2);
    expect(parsed.increases).toEqual([{ rule: 1, file: "src/a.svelte", before: 2, after: 3, isNew: false }]);
    expect(parsed.violations).toHaveLength(3);
  });

  it("--list --rule prints every hit of one rule", () => {
    run("--baseline");
    const r = run("--list", "--rule", "1");
    expect(r.out).toContain("src/a.svelte:1:");
    expect(r.out.match(/rule 1  /g)?.length).toBe(2);
  });

  it("rejects an unknown flag with exit 2", () => {
    expect(run("--nope").code).toBe(2);
  });

  it("skips tests, declaration files and generated files", () => {
    write("src/b.test.ts", 'export const c = "text-amber-400";\n');
    write("src/__tests__/c.ts", 'export const c = "text-amber-400";\n');
    write("src/d.d.ts", 'declare const c = "text-amber-400";\n');
    write("src/e.ts", '// @generated\nexport const c = "text-amber-400";\n');
    write("src/f.ts", 'export const c = "text-amber-400";\n');
    const { violations } = lintRepo({ root: dir });
    expect(violations.filter((x) => x.rule === 3).map((x) => x.file)).toEqual(["src/f.ts"]);
  });
});

describe("the committed baseline", () => {
  const baseline = readBaseline(DEFAULT_BASELINE_FILE);
  const repoRoot = path.resolve(fileURLToPath(new URL("..", import.meta.url)));

  it("is well formed: every rule present, totals equal the sum of the files, a full commit sha, and the retake note", () => {
    expect(Object.keys(baseline.rules).map(Number)).toEqual(RULES.map((r) => r.id));
    for (const rule of Object.values(baseline.rules)) {
      expect(rule.total).toBe(Object.values(rule.files).reduce((a, b) => a + b, 0));
    }
    expect(baseline.commit).toMatch(/^[0-9a-f]{40}$/);
    expect(baseline.retake).toContain("BASE_SHA");
  });

  it("matches a fresh run exactly when src/ is unchanged since the baseline commit, and otherwise never rises above it", () => {
    const fresh = countViolations(lintRepo({ root: repoRoot }).violations);
    const cmp = compareToBaseline(fresh, baseline);
    expect(cmp.increases).toEqual([]);
    const diff = spawnSync("git", ["diff", "--quiet", baseline.commit, "--", "src"], { cwd: repoRoot });
    if (diff.status === 0) {
      // Nothing under src/ moved since the baseline was taken: the counts must be identical,
      // so a stale baseline (a rule changed without retaking it) cannot hide a regression.
      expect(cmp.decreases).toEqual([]);
    }
  });
});
