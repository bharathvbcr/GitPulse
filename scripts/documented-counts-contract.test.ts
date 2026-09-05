import { execFileSync } from "node:child_process";
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
    for (const match of read(doc).matchAll(pattern)) {
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
   * These live only on the author's machine, so there is nothing to check on a
   * clean clone or in CI. `skipIf` reports that honestly — the reporter prints
   * the draft's name as skipped — where reading it unconditionally crashed the
   * three tests above, and where quietly dropping it from the corpus would
   * have looked exactly like a run that found nothing wrong.
   *
   * A draft that states no counts passes: there is no claim to be wrong.
   */
  for (const draft of LOCAL_DRAFTS) {
    it.skipIf(!existsSync(fileURLToPath(docUrl(draft))))(`${draft} matches the code`, () => {
      expectClaims([draft], HANDLER_CLAIM, handlerCount(), "handler", false);
      expectClaims([draft], VIEW_CLAIM, REGISTERED_VIEWS.length, "view", false);
      expectClaims([draft], FIELD_CLAIM, fieldCount(), "field", false);
    });
  }
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
   * The Rust watch table must be at least as large as the frontend's tab cap.
   *
   * They sit in different languages with no shared source, and today they are
   * both 24 — an exact fit with zero slack. If `MAX_OPEN_TABS` were ever
   * raised alone, a full workspace would deterministically leave its last
   * repositories unwatched: they would keep refreshing file statuses on the
   * poll while their branches, graph, parked-operation banner and stash went
   * stale, which is the failure `watchState` exists to make visible. Better to
   * fail the build than to ship a workspace with a guaranteed blind spot.
   */
  it("gives the watch table at least one slot per openable tab", () => {
    const watcher = read("src-tauri/src/watcher/mod.rs");
    const rustCap = /pub const MAX_WATCHES:\s*usize\s*=\s*(\d+)/.exec(watcher);
    expect(rustCap, "MAX_WATCHES must be findable in the watcher").toBeTruthy();

    const tabs = read("src/lib/repos/tabModel.ts");
    const tsCap = /export const MAX_OPEN_TABS\s*=\s*(\d+)/.exec(tabs);
    expect(tsCap, "MAX_OPEN_TABS must be findable in the tab model").toBeTruthy();

    const maxWatches = Number(rustCap![1]);
    const maxTabs = Number(tsCap![1]);
    expect(maxWatches).toBeGreaterThan(0);
    expect(
      maxWatches,
      `MAX_WATCHES (${maxWatches}) must be >= MAX_OPEN_TABS (${maxTabs}), or a ` +
        `full workspace is guaranteed to contain unwatched repositories`,
    ).toBeGreaterThanOrEqual(maxTabs);
  });
});
