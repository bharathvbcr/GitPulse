import { existsSync, readFileSync, readdirSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { parseRegisteredHandlers, DEFAULT_LIB_RS } from "./check-ipc-contract.mjs";
import * as checkCoverageTypes from "./check-coverage-types.mjs";
import { REGISTERED_VIEWS } from "../src/lib/views/viewRegistry";

/**
 * README, CONTRIBUTING and ARCHITECTURE state precise counts — 95 IPC
 * handlers, 357 compared fields, 13 views. Numbers in prose rot silently: the
 * Rust test count sat at "~200+" while the real figure was 890.
 *
 * Each count is taken from the canonical implementation rather than recounted
 * here, so this cannot drift from what the checkers actually measure.
 */
/**
 * Documents that ship with the repository. A missing one is a hard failure:
 * a tracked doc was deleted and the contract has to say so.
 */
const REPO_DOCS = [
  "README.md",
  "CONTRIBUTING.md",
  "docs/ARCHITECTURE.md",
  "docs/FEATURES.md",
] as const;

/**
 * Local-only drafts, gitignored under "internal notes and promotional drafts".
 *
 * PROMO.md joined this contract after its "13 Purpose-Built Views" survived two
 * consolidations unnoticed: it was outside the contract, so nothing read it.
 * But it is not IN the repository, so requiring it made this suite pass only on
 * a machine that happened to have a copy, and fail on every clean clone.
 *
 * It is still checked wherever it exists. Where it does not, the test below is
 * reported as SKIPPED rather than passing — a check that could not run must
 * never look like one that ran and found nothing wrong.
 */
const LOCAL_DOCS = ["docs/PROMO.md"] as const;

const docUrl = (relative: string) => new URL(`../${relative}`, import.meta.url);

function read(relative: string): string {
  return readFileSync(docUrl(relative), "utf8");
}

/** The documents this run actually reads; local drafts count when present. */
function presentDocs(): string[] {
  return [...REPO_DOCS, ...LOCAL_DOCS].filter((doc) => existsSync(docUrl(doc)));
}

/** Every distinct number asserted about `thing` across the docs. */
function claimedCounts(pattern: RegExp): Map<string, Set<number>> {
  const found = new Map<string, Set<number>>();
  for (const doc of presentDocs()) {
    for (const match of read(doc).matchAll(pattern)) {
      const value = Number(match[1].replace(/,/g, ""));
      if (!Number.isFinite(value)) continue;
      if (!found.has(doc)) found.set(doc, new Set());
      found.get(doc)?.add(value);
    }
  }
  return found;
}

/**
 * Every count pattern is case-INSENSITIVE.
 *
 * They were not, and the handler pattern compensated by listing "Registered
 * Handlers" and "Handlers" beside "handlers" by hand — a workaround the views
 * and fields patterns never got. So a heading or table cell claiming
 * "13 Purpose-Built Views" matched nothing and passed, which is the exact
 * phrasing this contract was extended to catch.
 */
function expectAllClaim(pattern: RegExp, actual: number, label: string): void {
  const claims = claimedCounts(pattern);
  expect(claims.size, `no document states a ${label} count`).toBeGreaterThan(0);
  for (const [doc, values] of claims) {
    for (const value of values) {
      expect(value, `${doc} claims ${value} ${label}; the code has ${actual}`).toBe(actual);
    }
  }
}

describe("documented counts match the code", () => {
  it("reads every document that ships with the repository", () => {
    // Every count assertion below is vacuous over a document that was not
    // read, so which documents are readable is itself part of the contract.
    for (const doc of REPO_DOCS) {
      expect(existsSync(docUrl(doc)), `${doc} is part of the repository but missing`).toBe(
        true,
      );
    }
    expect(presentDocs()).toEqual(expect.arrayContaining([...REPO_DOCS]));
  });

  it("states the real number of IPC handlers", () => {
    // Counted by the IPC checker's own parser, not by a second regex here.
    const { handlers, errors } = parseRegisteredHandlers(readFileSync(DEFAULT_LIB_RS, "utf8"));
    expect(errors.length, "the registry must parse cleanly for the count to mean anything").toBe(0);
    const actual = handlers.size;
    expect(actual).toBeGreaterThan(50);
    expectAllClaim(/(\d+)\s+(?:rust commands|registered handlers|handlers)/gi, actual, "handler");
  });

  it("states the real number of registered views", () => {
    expectAllClaim(
      /(\d+)\s+(?:application |specialized |purpose-built )?views/gi,
      REGISTERED_VIEWS.length,
      "view",
    );
  });

  it("states the real number of type-checked fields", () => {
    // Sum the contracts the checker actually compares, via its own JSON mode.
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
    expectAllClaim(/(\d+)\s+(?:data )?fields/gi, actual, "field");
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

describe.each(LOCAL_DOCS)("%s, a local-only draft", (doc) => {
  // Skipped, not passed, when the draft is absent: the run then says out loud
  // that this document went unchecked instead of implying it was clean.
  it.runIf(existsSync(docUrl(doc)))("is covered by the count contract while it exists", () => {
    expect(presentDocs()).toContain(doc);
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
