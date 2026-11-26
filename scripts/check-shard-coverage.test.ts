import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { afterEach, describe, expect, it, vi } from "vitest";
import { CouldNotRun, compare, expectedTests, main, ranTests } from "./check-shard-coverage.mjs";

const listing = (suites: Record<string, Record<string, { ignored?: boolean; status?: string }>>) => ({
  "test-count": 0,
  "rust-suites": Object.fromEntries(
    Object.entries(suites).map(([binary, cases]) => [
      binary,
      {
        "binary-id": binary,
        testcases: Object.fromEntries(
          Object.entries(cases).map(([name, c]) => [
            name,
            { ignored: c.ignored ?? false, "filter-match": { status: c.status ?? "matches" } },
          ]),
        ),
      },
    ]),
  ),
});

const report = (cases: [string, string][]) =>
  `<?xml version="1.0" encoding="UTF-8"?><testsuites name="nextest-run">${cases
    .map(([binary, name]) => `<testsuite name="${binary}"><testcase name="${name}" classname="${binary}" time="0.1"/></testsuite>`)
    .join("")}</testsuites>`;

const SUITES = { waylode_lib: { "a::one": {}, "a::two": {}, "b::three": {} }, native: { native: {} } };

describe("expectedTests", () => {
  it("lists what a full run would execute and leaves out #[ignore]d tests", () => {
    const expected = expectedTests(listing({ lib: { run: {}, slow: { ignored: true } } }));
    expect([...expected]).toEqual(["lib\trun"]);
  });

  it("leaves out tests the filter does not match", () => {
    expect(expectedTests(listing({ lib: { run: {}, other: { status: "mismatch" } } })).size).toBe(1);
  });

  it.each([
    ["no rust-suites", {}],
    ["a suite without testcases", { "rust-suites": { lib: {} } }],
    ["a test without filter-match", { "rust-suites": { lib: { testcases: { t: { ignored: false } } } } }],
    ["no runnable test", listing({ lib: { only: { ignored: true } } })],
  ])("could not run on %s, rather than expecting nothing", (_, document) => {
    expect(() => expectedTests(document)).toThrow(CouldNotRun);
  });
});

describe("ranTests", () => {
  it("reads classname and name, unescaping entities", () => {
    expect(ranTests(report([["lib", "t::a&amp;b &quot;q&quot;"]]))).toEqual(['lib\tt::a&b "q"']);
  });

  it("reads a testcase that has children, as a failed one does", () => {
    const xml = `<testsuites><testsuite name="lib"><testcase name="t" classname="lib"><failure type="test failure">x</failure></testcase></testsuite></testsuites>`;
    expect(ranTests(xml)).toEqual(["lib\tt"]);
  });

  it("is not a report without a testsuite, and not a report with a nameless testcase", () => {
    expect(() => ranTests("<html/>")).toThrow(CouldNotRun);
    expect(() => ranTests(`<testsuites><testcase name="t"/></testsuites>`)).toThrow(CouldNotRun);
  });

  it("unescapes numeric entities in one pass without double-decoding &", () => {
    expect(ranTests(report([["lib", "t::&#38;quot; &#x26;lt;"]]))).toEqual(["lib\tt::&quot; &lt;"]);
  });
});

describe("compare", () => {
  const expected = expectedTests(listing(SUITES));
  const all = [...expected];

  it("accepts a partition into disjoint buckets that covers the list", () => {
    expect(compare(expected, [all.slice(0, 2), all.slice(2, 3), all.slice(3)])).toEqual({ missing: [], unlisted: [], duplicated: [] });
  });

  it("names a test no shard ran, which is a shard that never ran", () => {
    const result = compare(expected, [all.slice(0, 2), all.slice(2, 3)]);
    expect(result.missing).toEqual(["native native"]);
  });

  it("names a test two shards both ran", () => {
    expect(compare(expected, [all, [all[0]]]).duplicated).toEqual(["waylode_lib a::one"]);
  });

  it("names a test that ran but was never listed", () => {
    expect(compare(expected, [[...all, "lib\tghost"]]).unlisted).toEqual(["lib ghost"]);
  });

  it("replaces every tab in a reported test id, not only the first", () => {
    expect(compare(expected, [[...all, "lib\tghost\textra"]]).unlisted).toEqual(["lib ghost extra"]);
  });
});

describe("main", () => {
  const dirs: string[] = [];
  afterEach(() => {
    vi.restoreAllMocks();
    for (const dir of dirs.splice(0)) rmSync(dir, { recursive: true, force: true });
  });

  const stage = (cases: [string, string][][], suites = SUITES) => {
    const dir = mkdtempSync(path.join(tmpdir(), "gitpulse-shards-"));
    dirs.push(dir);
    const list = path.join(dir, "list.json");
    writeFileSync(list, JSON.stringify(listing(suites)));
    const reports = cases.map((shard, i) => {
      const file = path.join(dir, `junit-${i}.xml`);
      writeFileSync(file, report(shard));
      return file;
    });
    return { dir, list, reports };
  };

  const run = (argv: string[]) => {
    vi.spyOn(console, "log").mockImplementation(() => {});
    vi.spyOn(console, "error").mockImplementation(() => {});
    return main(argv);
  };

  const FULL: [string, string][][] = [
    [["waylode_lib", "a::one"], ["native", "native"]],
    [["waylode_lib", "a::two"]],
    [["waylode_lib", "b::three"]],
  ];

  it("passes when the shards cover the list exactly once", () => {
    const { list, reports } = stage(FULL);
    expect(run(["--list", list, "--shards", "3", ...reports])).toBe(0);
  });

  it("fails, not could-not-run, when a shard's bucket is empty", () => {
    const { list, reports } = stage([FULL[0], FULL[1], []]);
    expect(run(["--list", list, "--shards", "3", ...reports])).toBe(1);
  });

  it("could not run when a shard report is absent", () => {
    const { list, reports } = stage(FULL);
    expect(run(["--list", list, "--shards", "3", ...reports.slice(0, 2)])).toBe(2);
    expect(run(["--list", list, "--shards", "3", ...reports.slice(0, 2), path.join(path.dirname(list), "nope.xml")])).toBe(2);
  });

  it("could not run on a list that is not JSON", () => {
    const { dir, reports } = stage(FULL);
    const list = path.join(dir, "bad.json");
    writeFileSync(list, "not json");
    expect(run(["--list", list, "--shards", "3", ...reports])).toBe(2);
  });

  it("could not run without --list and --shards", () => {
    expect(run([])).toBe(2);
  });
});
