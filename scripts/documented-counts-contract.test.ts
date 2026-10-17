import { execFileSync, spawnSync } from "node:child_process";
import { existsSync, readFileSync, readdirSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { parseRegisteredHandlers, DEFAULT_LIB_RS } from "./check-ipc-contract.mjs";
import * as checkCoverageTypes from "./check-coverage-types.mjs";
import { REGISTERED_VIEWS } from "../src/lib/views/viewRegistry";

/**
 * README, CONTRIBUTING and ARCHITECTURE state precise counts — 136 IPC
 * handlers, 762 compared fields, 4 views. Numbers in prose rot silently: the
 * Rust test count sat at "~200+" while the real figure was 890.
 *
 * Each count is taken from the canonical implementation rather than recounted
 * here, so this cannot drift from what the checkers actually measure.
 */
// This committed contract covers versioned docs. PROMO.md is ignored and
// absent from fresh checkouts; track it before bringing its counts under this
// contract. A local copy must never be required for these checks to run.
const TRACKED_DOCS = [
  "README.md",
  "CONTRIBUTING.md",
  "docs/ARCHITECTURE.md",
  "docs/FEATURES.md",
] as const;

/**
 * Promotional copy that `.gitignore` deliberately keeps out of the repository.
 *
 * PROMO.md was briefly listed beside the committed docs, which made every run
 * on a clean clone fail with ENOENT: no clone has the file, so a repository
 * contract cannot gate it. It is still worth checking where it exists — its
 * "13 Purpose-Built Views" survived two consolidations unnoticed — so each
 * draft gets its own test that `skipIf` reports as *skipped* when the file is
 * absent. A check that could not run must never look like one that passed.
 */
const LOCAL_DRAFTS = ["docs/PROMO.md"] as const;

/**
 * The checker's own SCOPE docstring, which states the same two counts.
 *
 * That docstring is where this rot started: it claimed "40 contracts over 57
 * structs" while its table held 57 and 142. A reader deciding whether their
 * payload is covered reads the note beside the table long before they read
 * README, so guarding only the documents would guard the echo and not the
 * source.
 *
 * Only the SCOPE note is scanned, not the whole file: prose patterns are far
 * too eager against source, where "Exit codes: 0 contract holds · 1 contract
 * violation" reads as two claims that the table has 0 and 1 contracts.
 */
const SCOPE_SOURCE = "scripts/check-coverage-types.mjs";

function scopeNote(): string {
  const source = read(SCOPE_SOURCE);
  const start = source.indexOf(" * SCOPE:");
  expect(start, `${SCOPE_SOURCE} no longer has a SCOPE note to check`).toBeGreaterThan(-1);
  // Runs to the end of that comment paragraph.
  const end = source.indexOf("\n *\n", start);
  return source.slice(start, end < 0 ? source.indexOf("*/", start) : end);
}

function docUrl(relative: string): URL {
  return new URL(`../${relative}`, import.meta.url);
}

function read(relative: string): string {
  return readFileSync(docUrl(relative), "utf8");
}

/**
 * Matches a claim of "<count> somethings": a number, up to three words of
 * prose, then the noun.
 *
 * The `(\d+)\s+views` pattern this replaces only saw a noun sitting
 * immediately after the number in lower case, which prose rarely does. It was
 * blind to README's "4 Specialized Views" and to all five stale claims in the
 * promotional draft it had just been pointed at — "13 Purpose-Built
 * Specialized Views" and three spellings of "95 … command handlers" — so the
 * drift this contract exists to catch would have passed green. The lookbehind
 * keeps section numbering ("### 4.2 View Switching") from reading as a claim
 * about 2 views; a trailing "+" marks a floor rather than an exact figure.
 */
function claimPattern(noun: string): RegExp {
  return new RegExp(
    String.raw`(?<![\d.])(\d[\d,]*)(\+)?(?:\s+[A-Za-z][\w-]*){0,3}\s+(?:${noun})\b`,
    "gi",
  );
}

const HANDLER_CLAIM = claimPattern("handlers?|commands?");
const VIEW_CLAIM = claimPattern("views?");
const FIELD_CLAIM = claimPattern("fields?");
const CONTRACT_CLAIM = claimPattern("contracts?");
const STRUCT_CLAIM = claimPattern("structs?");

interface Claim {
  doc: string;
  value: number;
  floor: boolean;
  text: string;
}

/** Every number asserted about `pattern`'s subject across `docs`. */
function claimsIn(pattern: RegExp, docs: readonly string[]): Claim[] {
  const found: Claim[] = [];
  for (const doc of docs) {
    // The scope note is a slice of a source file, not a whole document.
    const text = doc === SCOPE_SOURCE ? scopeNote() : read(doc);
    for (const match of text.matchAll(pattern)) {
      const value = Number(match[1].replace(/,/g, ""));
      if (!Number.isFinite(value)) continue;
      found.push({ doc, value, floor: match[2] === "+", text: match[0].trim() });
    }
  }
  return found;
}

/** Assert each claim in `docs`; `require` demands the docs make one at all. */
function expectClaims(
  docs: readonly string[],
  pattern: RegExp,
  actual: number,
  label: string,
  require = true,
): void {
  const found = claimsIn(pattern, docs);
  if (require) expect(found.length, `no document states a ${label} count`).toBeGreaterThan(0);
  for (const claim of found) {
    const why = `${claim.doc} says "${claim.text}"; the code has ${actual} ${label}s`;
    if (claim.floor) expect(actual, why).toBeGreaterThanOrEqual(claim.value);
    else expect(claim.value, why).toBe(actual);
  }
}

/** Counted by the IPC checker's own parser, not by a second regex here. */
function handlerCount(): number {
  const { handlers, errors } = parseRegisteredHandlers(readFileSync(DEFAULT_LIB_RS, "utf8"));
  expect(errors.length, "the registry must parse cleanly for the count to mean anything").toBe(0);
  return handlers.size;
}

/** Sum the contracts the checker actually compares, via its own JSON mode. */
function fieldCount(): number {
  const logged: string[] = [];
  const original = console.log;
  console.log = (...args: unknown[]) => void logged.push(args.join(" "));
  let code: number;
  try {
    code = checkCoverageTypes.main(["--json"]);
  } finally {
    console.log = original;
  }
  expect(code).toBe(0);
  const reports = JSON.parse(logged.join("\n")) as Array<{ fieldCount?: number }>;
  const actual = reports.reduce((sum, report) => sum + (report.fieldCount ?? 0), 0);
  expect(actual, "the checker should report a field count").toBeGreaterThan(0);
  return actual;
}

/** How many contracts the type checker compares, from its own table. */
function contractCount(): number {
  const actual = checkCoverageTypes.CONTRACTS.length;
  expect(actual, "the checker should define contracts").toBeGreaterThan(0);
  return actual;
}

/** How many structs those contracts cover, from the same table. */
function structCount(): number {
  const actual = checkCoverageTypes.CONTRACTS.reduce(
    (sum: number, contract: { structs: readonly string[] }) => sum + contract.structs.length,
    0,
  );
  expect(actual, "the checker's contracts should name structs").toBeGreaterThan(0);
  return actual;
}

describe("documented counts match the code", () => {
  /**
   * A document that stops being read is indistinguishable from one whose
   * numbers are all correct, and that is how PROMO.md's absence turned into a
   * crash rather than a finding. Naming the corpus, and requiring every member
   * to still be making a claim, keeps a rename or a rewrite from quietly
   * checking three documents where the suite reports four.
   */
  it("reads every committed document it claims to check", () => {
    for (const doc of TRACKED_DOCS) {
      expect(existsSync(fileURLToPath(docUrl(doc))), `${doc} is missing`).toBe(true);
    }
    const stating = [HANDLER_CLAIM, VIEW_CLAIM, FIELD_CLAIM].flatMap((pattern) =>
      claimsIn(pattern, TRACKED_DOCS).map((claim) => claim.doc),
    );
    expect(
      [...new Set(stating)].sort(),
      "every committed document must state one of the counts this contract checks",
    ).toEqual([...TRACKED_DOCS].sort());
  });

  /**
   * Existing on this machine is weaker than being tracked: an ignored or
   * untracked local copy satisfies `existsSync` and is absent from every fresh
   * clone, which is the shape PROMO.md had when it crashed CI. Asking git,
   * rather than the filesystem, is what makes the corpus a repository fact.
   */
  it("only depends on tracked documents available in fresh checkouts", () => {
    const tracked = new Set(
      execFileSync("git", ["ls-files", "-z", "--", ...TRACKED_DOCS], {
        cwd: fileURLToPath(new URL("..", import.meta.url)),
        encoding: "utf8",
        timeout: 5_000,
      }).split("\0"),
    );
    expect(
      TRACKED_DOCS.filter((doc) => !tracked.has(doc)),
      "counted documents must be tracked; local or ignored files cannot satisfy the contract",
    ).toEqual([]);
  });

  it("rejects a count check when no document makes a matching claim", () => {
    // The negative lookahead never matches, regardless of document contents.
    expect(() => expectClaims(TRACKED_DOCS, /(\d+)(?!)/g, 0, "handler")).toThrow(
      "no document states a handler count",
    );
  });

  it("states the real number of IPC handlers", () => {
    const actual = handlerCount();
    expect(actual).toBeGreaterThan(50);
    expectClaims(TRACKED_DOCS, HANDLER_CLAIM, actual, "handler");
  });

  it("states the real number of registered views", () => {
    expectClaims(TRACKED_DOCS, VIEW_CLAIM, REGISTERED_VIEWS.length, "view");
  });

  /**
   * Added after both of these were found stale in three documents at once, and
   * in the checker's own docstring: it claimed "40 contracts over 57 structs"
   * when the table held 57 contracts and 142 structs. Handlers, views and
   * fields were guarded; these two were not, so they were the two that rotted.
   */
  it("states the real number of type contracts", () => {
    expectClaims([...TRACKED_DOCS, SCOPE_SOURCE], CONTRACT_CLAIM, contractCount(), "contract");
  });

  it("states the real number of contracted structs", () => {
    expectClaims([...TRACKED_DOCS, SCOPE_SOURCE], STRUCT_CLAIM, structCount(), "struct");
  });

  it("states the real number of type-checked fields", () => {
    expectClaims(TRACKED_DOCS, FIELD_CLAIM, fieldCount(), "field");
  });

  it("states test-suite sizes as floors, so growth does not make them false", () => {
    // "1,890+" went stale as an exact-ish figure; a floor stays true while the
    // suite grows and only breaks if coverage genuinely shrinks.
    const contributing = read("CONTRIBUTING.md");
    expect(contributing).toContain("2,000+ tests");
    expect(contributing).toContain("850+ tests");
    expect(contributing).not.toContain("~200+");
  });
});

describe("promotional drafts state the real counts too", () => {
  /**
   * `skipIf` below is honest only for a file git ignores. A draft that is
   * tracked (or merely not ignored) and missing is a deleted or renamed
   * document, and skipping it would look exactly like a clean run, so that case
   * fails instead.
   */
  for (const draft of LOCAL_DRAFTS) {
    it(`${draft} may be absent only while git ignores it`, () => {
      if (existsSync(fileURLToPath(docUrl(draft)))) return;
      const root = fileURLToPath(new URL("..", import.meta.url));
      const ignored = spawnSync("git", ["check-ignore", "-q", "--", draft], {
        cwd: root,
        timeout: 5_000,
      });
      expect(ignored.error, "git check-ignore could not run").toBeUndefined();
      expect(
        ignored.status,
        `${draft} is missing and not ignored: restore it or drop it from LOCAL_DRAFTS`,
      ).toBe(0);
    });
  }

  /**
   * These live only on the author's machine, so their result must never decide
   * the repository's verdict: the same commit passed on a clean clone and
   * failed on a checkout whose ignored PROMO.md had gone stale. The check is
   * therefore opt-in (`GITPULSE_CHECK_LOCAL_DRAFTS=1`), and `skipIf` reports it
   * as skipped otherwise, never as passed. Run it before publishing a draft.
   *
   * A draft that states no counts passes: there is no claim to be wrong.
   */
  for (const draft of LOCAL_DRAFTS) {
    it.skipIf(process.env.GITPULSE_CHECK_LOCAL_DRAFTS !== "1" || !existsSync(fileURLToPath(docUrl(draft))))(`${draft} matches the code`, () => {
      expectClaims([draft], HANDLER_CLAIM, handlerCount(), "handler", false);
      expectClaims([draft], VIEW_CLAIM, REGISTERED_VIEWS.length, "view", false);
      expectClaims([draft], FIELD_CLAIM, fieldCount(), "field", false);
    });
  }
});


describe("documentation does not cite files that will not exist", () => {
  const root = fileURLToPath(new URL("..", import.meta.url));
  const tracked = execFileSync("git", ["ls-files", "-z", "--", "*.md"], {
    cwd: root,
    encoding: "utf8",
    timeout: 5_000,
  })
    .split("\0")
    // Dated ledgers and the changelog record what was true when written, and
    // are not claims about the present tree.
    .filter((doc) => doc && !doc.startsWith("docs/archive/") && doc !== "CHANGELOG.md");

  /** A `/tmp/<name>.log`-style path cited as evidence: cleaned by the OS, never committed. */
  const TMP_EVIDENCE = /\/tmp\/[\w.-]+\.(?:log|json|jsonl|txt)\b/g;

  it("scans the tracked Markdown it claims to", () => {
    expect(tracked).toContain("README.md");
    expect(tracked).toContain("docs/PERFORMANCE.md");
  });

  it("recognises a /tmp evidence citation", () => {
    expect("see `/tmp/gitpulse-ci.log` for the run".match(TMP_EVIDENCE)).toHaveLength(1);
    expect("HOME=/private/tmp/gpe-perf and /tmp/".match(TMP_EVIDENCE)).toBeNull();
  });

  it("cites no /tmp log as evidence", () => {
    const offenders = tracked.flatMap((doc) =>
      [...read(doc).matchAll(TMP_EVIDENCE)].map((m) => `${doc}: ${m[0]}`),
    );
    expect(
      offenders,
      "summarise the numbers inline or commit the evidence; /tmp is cleaned",
    ).toEqual([]);
  });
});

describe("the MCP server's documented surface matches its code", () => {
  const SKILL = "plugins/gitpulse/skills/gitpulse-insights/SKILL.md";
  const MANIFESTS = [
    ".claude-plugin/marketplace.json",
    "plugins/gitpulse/plugin.json",
    "plugins/gitpulse/.codex-plugin/plugin.json",
    "plugins/gitpulse/.cursor-plugin/plugin.json",
    "plugins/gitpulse/.claude-plugin/plugin.json",
  ] as const;
  const WRITE_TOOLS = [
    "gitpulse_add_task",
    "gitpulse_import_tasks",
    "gitpulse_complete_task",
    "gitpulse_merge_tasks",
    "gitpulse_delete_task",
  ] as const;

  it("states the real number of devmap_* tools", () => {
    const names = [...read("src-tauri/src/mcp/devmap_parity.rs")
      .match(/TOOL_NAMES: &\[&str\] = &\[([^\]]*)\]/)![1]
      .matchAll(/"(devmap_\w+)"/g)];
    expect(names.length).toBeGreaterThan(0);
    const claim = /\((\d+) tools:/.exec(read(SKILL));
    expect(claim, `${SKILL} must state how many devmap_* tools it maps`).toBeTruthy();
    expect(Number(claim![1]), `${SKILL} says ${claim![1]}; the code lists ${names.length}`).toBe(
      names.length,
    );
  });

  it("names only write tools the server defines, and calls the five its only writers", () => {
    const skill = read(SKILL);
    const server = read("src-tauri/src/mcp/mod.rs");
    for (const name of WRITE_TOOLS) {
      expect(skill, `${SKILL} omits ${name}`).toContain(name);
      expect(server, `${name} is not defined by the MCP server`).toContain(`"${name}"`);
    }
  });

  it("does not describe the server as read-only in any plugin manifest", () => {
    for (const manifest of MANIFESTS) {
      expect(read(manifest), `${manifest} calls a server with write tools read-only`).not.toMatch(
        /read-only/i,
      );
    }
  });
});

describe("the contract-test table in CONTRIBUTING stays honest", () => {
  // A table listing the safety net is itself a claim that can drift: a test
  // deleted still listed, or one added and never mentioned, and the table
  // quietly stops describing what protects the repository.
  const contributing = read("CONTRIBUTING.md");
  const files = readdirSync(fileURLToPath(new URL(".", import.meta.url)))
    .filter((name) => name.endsWith(".test.ts"))
    .map((name) => name.replace(/\.test\.ts$/, ""));

  /**
   * Tests that belong to a script rather than being a contract of their own.
   *
   * Derived, not listed: a `foo.test.ts` sitting beside a `foo.mjs` is that
   * script's unit test. The list this replaced had to be edited by hand every
   * time a script gained tests, and the entry most likely to be forgotten was
   * the newest one — so the table would start demanding a contract row for a
   * plain unit test, and the honest fix would look like noise.
   */
  const SCRIPT_UNIT_TESTS = new Set(
    files.filter((name) =>
      existsSync(fileURLToPath(new URL(`./${name}.mjs`, import.meta.url))),
    ),
  );
  // vite-config tests vite.config.ts, the one script-shaped test whose subject
  // is not a sibling .mjs.
  SCRIPT_UNIT_TESTS.add("vite-config");

  it("names only tests that exist", () => {
    const named = [...contributing.matchAll(/`([a-z-]+-contract|release-workflow)`/g)].map(
      (m) => m[1],
    );
    expect(named.length).toBeGreaterThan(10);
    for (const name of new Set(named)) {
      expect(files, `CONTRIBUTING names ${name}, which does not exist`).toContain(name);
    }
  });

  it("mentions every contract test that is not a script's own unit test", () => {
    const undocumented = files.filter(
      (name) =>
        !SCRIPT_UNIT_TESTS.has(name) &&
        !contributing.includes(`\`${name}\``),
    );
    expect(undocumented).toEqual([]);
  });
});

describe("cross-boundary caps", () => {
  /**
   * The Rust watch table must hold the frontend's whole watch pool, with room
   * to spare.
   *
   * They sit in different languages with no shared source. Open tabs are not
   * bounded by watches any more — the pool (src/lib/repos/watchPool.ts) holds
   * the most recently used repositories and parks the rest — so the pool is
   * what has to fit. Strictly, with slack: an eviction admits the newcomer
   * before the evicted session finishes retiring, so the table briefly holds
   * one more than the pool, and an exact fit would refuse the repository a
   * person just opened whenever a slot leaked.
   */
  it("gives the watch table more slots than the watch pool holds", () => {
    const watcher = read("src-tauri/src/watcher/mod.rs");
    const rustCap = /pub const MAX_WATCHES:\s*usize\s*=\s*(\d+)/.exec(watcher);
    expect(rustCap, "MAX_WATCHES must be findable in the watcher").toBeTruthy();

    const pool = read("src/lib/repos/watchPool.ts");
    const poolCap = /export const WATCH_POOL_SIZE\s*=\s*(\d+)/.exec(pool);
    expect(poolCap, "WATCH_POOL_SIZE must be findable in the watch pool").toBeTruthy();

    const maxWatches = Number(rustCap![1]);
    const poolSize = Number(poolCap![1]);
    expect(poolSize).toBeGreaterThan(0);
    expect(
      maxWatches,
      `MAX_WATCHES (${maxWatches}) must exceed WATCH_POOL_SIZE (${poolSize}), or an ` +
        `eviction can refuse the repository being opened`,
    ).toBeGreaterThan(poolSize);
  });

  it("no longer bounds open tabs by the watch table", () => {
    const tabs = read("src/lib/repos/tabModel.ts");
    const tsCap = /export const MAX_OPEN_TABS\s*=\s*(\d+)/.exec(tabs);
    expect(tsCap, "MAX_OPEN_TABS must be findable in the tab model").toBeTruthy();
    const watcher = read("src-tauri/src/watcher/mod.rs");
    const rustCap = Number(/pub const MAX_WATCHES:\s*usize\s*=\s*(\d+)/.exec(watcher)![1]);
    expect(Number(tsCap![1])).toBeGreaterThan(rustCap);
  });
});
