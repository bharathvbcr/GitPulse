import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { parseRustStructs } from "./check-coverage-types.mjs";

/**
 * `gh ... --json a,b,c` is a wire contract with an external tool, and it fails
 * in two directions that look nothing alike.
 *
 * Ask for a field gh does not know and the WHOLE listing errors out — that
 * once degraded the entire GitHub panel, which is why the pull-request list
 * has been pinned since. Ask for too *few* and nothing errors at all: the
 * struct field gh was never asked for deserializes to its default, so a title
 * renders empty or a timestamp reads as the epoch, silently and forever.
 *
 * Only the pull-request and release listings were pinned. This covers every
 * listing, in both directions.
 */
const SOURCES = {
  "github/mod.rs": fileURLToPath(new URL("../src-tauri/src/github/mod.rs", import.meta.url)),
  "github/actions.rs": fileURLToPath(new URL("../src-tauri/src/github/actions.rs", import.meta.url)),
};

/**
 * gh's own vocabulary, printed by running each subcommand's `--json` with no
 * value. Captured from gh 2.95.0 rather than recalled: an invented list here
 * would assert nothing while looking like it asserted everything.
 */
const GH_FIELDS: Record<string, string[]> = {
  issue: "assignees author blockedBy blocking body closed closedAt closedByPullRequestsReferences comments createdAt id isPinned issueType labels milestone number parent projectCards projectItems reactionGroups state stateReason subIssues subIssuesSummary title updatedAt url".split(" "),
  run: "attempt conclusion createdAt databaseId displayTitle event headBranch headSha name number startedAt status updatedAt url workflowDatabaseId workflowName".split(" "),
  workflow: "id name path state".split(" "),
};

/** Each listing: where its argv is built, and the struct that parses the reply. */
const LISTINGS = [
  { subcommand: "issue", fn: "list_issues", source: "github/mod.rs", struct: "GhIssue" },
  { subcommand: "run", fn: "list_workflow_runs", source: "github/mod.rs", struct: "GhWorkflowRun" },
  {
    subcommand: "workflow",
    fn: "workflow_list_leading_args",
    source: "github/actions.rs",
    struct: "GhWorkflow",
  },
] as const;

/** The comma-separated field list passed to `--json` inside `fn`. */
function requestedFields(source: string, fn: string): string[] {
  const start = source.indexOf(`fn ${fn}`);
  expect(start, `${fn} must exist`).toBeGreaterThanOrEqual(0);
  const marker = source.indexOf('"--json"', start);
  expect(marker, `${fn} must pass --json`).toBeGreaterThanOrEqual(0);
  const literal = /"([^"]+)"/.exec(source.slice(marker + '"--json"'.length));
  expect(literal, `${fn} must pass a field list after --json`).not.toBeNull();
  return (literal?.[1] ?? "").split(",").map((field) => field.trim());
}

/** Wire keys the struct expects, honouring `#[serde(rename)]`. */
function structWireKeys(source: string, name: string): string[] {
  const parsed = parseRustStructs(source, [name], { requirePub: false });
  expect(parsed.violations, `parsing ${name}`).toEqual([]);
  return [...(parsed.structs.get(name)?.fields.keys() ?? [])];
}

describe("gh --json field lists match both gh and the structs that parse them", () => {
  const read = (key: string) => readFileSync(SOURCES[key as keyof typeof SOURCES], "utf8");

  it.each(LISTINGS.map((l) => [l.subcommand, l] as const))(
    "%s list: asks only for fields gh knows",
    (_name, listing) => {
      const fields = requestedFields(read(listing.source), listing.fn);
      expect(fields.length).toBeGreaterThan(0);
      const unknown = fields.filter((field) => !GH_FIELDS[listing.subcommand].includes(field));
      expect(
        unknown,
        `gh ${listing.subcommand} list does not know these; the whole listing would fail`,
      ).toEqual([]);
    },
  );

  it.each(LISTINGS.map((l) => [l.subcommand, l] as const))(
    "%s list: asks for exactly what its struct reads",
    (_name, listing) => {
      const source = read(listing.source);
      const requested = new Set(requestedFields(source, listing.fn));
      const expected = structWireKeys(source, listing.struct);

      // The silent direction: a field the struct reads but never requests
      // deserializes to its default, so the value is simply blank.
      const neverRequested = expected.filter((key) => !requested.has(key));
      expect(
        neverRequested,
        `${listing.struct} reads these but gh is never asked for them; they would deserialize to defaults`,
      ).toEqual([]);

      // The loud-but-wasteful direction: requested and then dropped, which
      // usually means a typo or a field whose use was removed.
      const unread = [...requested].filter((field) => !expected.includes(field));
      expect(unread, `requested but ${listing.struct} has no field for them`).toEqual([]);
    },
  );

  it("keeps the gh vocabularies non-empty", () => {
    // A cleared list would make the first assertion vacuous.
    for (const [subcommand, fields] of Object.entries(GH_FIELDS)) {
      expect(fields.length, `${subcommand} vocabulary`).toBeGreaterThan(3);
    }
  });
});
