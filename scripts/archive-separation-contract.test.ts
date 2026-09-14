import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

/**
 * `docs/ARCHIVE_SEPARATION.md` explains why a task cannot carry an `archived`
 * flag: the vendored `dc-store` has no column to store it, no `items.put`
 * field to write it and no `items.list` filter to read it. Every one of those
 * is a claim about a file in this repository, and a plan whose premise has
 * quietly stopped being true is worse than no plan — it is a standing
 * instruction to do work that is already done, or to avoid work that is now
 * possible.
 *
 * So this pins the three of them. **A failure here is good news**: it means
 * upstream landed the field, and the fix is to execute the plan and delete the
 * document, not to relax the test.
 *
 * It also pins the seam from the other side. The plan promises that switching
 * costs four functions in one module; that promise only holds while nothing
 * else in the app decides for itself what "archived" means.
 */
const url = (path: string) => new URL(`../${path}`, import.meta.url);
const read = (path: string) => readFileSync(url(path), "utf8");

const STORE = "src-tauri/vendored/dc-store/src/workbench/mod.rs";
const SCHEMA = "src-tauri/vendored/dc-store/src/workbench/schema.sql";
const plan = read("docs/ARCHIVE_SEPARATION.md");
const store = read(STORE);
const schema = read(SCHEMA);

/** The `input.fields(&[...])` list at the top of one function body. */
function acceptedFields(source: string, fn: string): string[] {
  const start = source.indexOf(`fn ${fn}(`);
  expect(start, `${STORE} must define ${fn}`).toBeGreaterThan(-1);
  const body = source.slice(start);
  const open = body.indexOf("input.fields(&[");
  expect(open, `${fn} must validate its accepted fields`).toBeGreaterThan(-1);
  const close = body.indexOf("])", open);
  return [...body.slice(open, close).matchAll(/"([a-z_]+)"/g)].map((match) => match[1]);
}

/** The column names of one `CREATE TABLE`. */
function columns(sql: string, table: string): string[] {
  const start = sql.indexOf(`CREATE TABLE ${table} (`);
  expect(start, `${SCHEMA} must define ${table}`).toBeGreaterThan(-1);
  const body = sql.slice(start, sql.indexOf(");", start));
  return [...body.matchAll(/(?:^|,)\s*([a-z_]+)\s+(?:TEXT|INTEGER)/g)].map((match) => match[1]);
}

describe("the premise of the archive-separation plan", () => {
  it("reads the store it is asserting about", () => {
    // Guards against the checks below passing because a rename made every
    // lookup return nothing.
    expect(acceptedFields(store, "put_item")).toContain("status");
    expect(acceptedFields(store, "list_items")).toContain("status");
    expect(columns(schema, "work_items")).toContain("status");
    expect(columns(schema, "work_workspaces")).toContain("archived");
  });

  it("still cannot store an archived flag on a task", () => {
    expect(
      columns(schema, "work_items"),
      "work_items grew `archived` — execute docs/ARCHIVE_SEPARATION.md and delete it",
    ).not.toContain("archived");
  });

  it("still cannot write one", () => {
    const fields = acceptedFields(store, "put_item");
    expect(fields, "items.put now accepts `archived` — execute the plan").not.toContain("archived");
    // The plan's exact count, so "eighteen named fields" cannot go stale.
    expect(fields.length).toBe(18);
    expect(plan).toContain("eighteen accepted names");
  });

  it("still cannot filter a list by one", () => {
    const fields = acceptedFields(store, "list_items");
    expect(fields, "items.list now filters on `archived` — execute the plan").not.toContain("archived");
    // The plan quotes this list; an added filter would make the quote false
    // even if it were not `archived`.
    expect(fields).toEqual(["limit", "cursor", "workspace_id", "repository_id", "status", "query"]);
    for (const field of fields) expect(plan).toContain(`\`${field}\``);
  });

  it("names the schema version the migration would follow", () => {
    const terminal = /if version != (\d+) \{/.exec(store)?.[1];
    expect(terminal, "the migration ladder must end in a supported-version check").toBeDefined();
    expect(plan).toContain(`Schema version is **${terminal}**; this is the migration to ${Number(terminal) + 1}.`);
  });

  it("names the vendored commit the plan was written against", () => {
    const vendor = JSON.parse(read("src-tauri/vendored/VENDOR.json"));
    const dcStore = vendor.crates.find((crate: { name: string }) => crate.name === "dc-store");
    expect(dcStore, "VENDOR.json must describe dc-store").toBeDefined();
    expect(plan).toContain(dcStore.origin.commit.slice(0, 7));
  });
});

describe("the seam the plan promises to switch", () => {
  const archive = read("src/lib/workbench/taskArchive.ts");

  it("keeps every function the plan says it will change", () => {
    for (const owner of ["isArchived", "archiveAction", "restoreAction", "ARCHIVE_RULE", "boardPresence"]) {
      expect(archive, `taskArchive.ts must export ${owner}`).toContain(`export ${owner.startsWith("ARCHIVE") ? "const" : "function"} ${owner}`);
      expect(plan, `the plan must account for ${owner}`).toContain(owner);
    }
  });

  /**
   * The whole value of the seam. One module decides what archived means; if a
   * second surface starts comparing a status to the archive itself, switching
   * stops being a four-function change and the plan above becomes fiction.
   */
  it("is the only place in the app that decides what archived means", () => {
    const surfaces = [
      "src/lib/components/TaskArchive.svelte",
      "src/lib/components/TaskBoard.svelte",
      "src/lib/components/TaskContextMenu.svelte",
      "src/lib/workbench/taskMenu.ts",
      "src/lib/ui/taskView.ts",
    ];
    /**
     * A *task status* spelled `"done"`, not the word.
     *
     * `"done"` is also an `ActionState` — `row.state === "done"` means a batch
     * entry finished — and a rule keyed on the literal alone fails the board
     * for a line about something else entirely. The token beside the literal
     * is what separates the two vocabularies.
     */
    const spelledStatus =
      /(?:\bstatus\b\s*[!=]==?\s*|\bstatus\s*:\s*|STATUS_LABELS\s*\[\s*)["']done["']|["']done["']\s*[!=]==?\s*[\w.]*\bstatus\b/;
    /** The decision `isArchived` owns, made where it cannot be switched from. */
    const handRolled = /\bstatus\b\s*[!=]==?\s*ARCHIVE_STATUS|ARCHIVE_STATUS\s*[!=]==?\s*[\w.]*\bstatus\b/;
    for (const path of surfaces) {
      const source = read(path);
      expect(source, `${path} must not spell the archived status`).not.toMatch(spelledStatus);
      expect(source, `${path} must ask isArchived, not compare to ARCHIVE_STATUS`).not.toMatch(handRolled);
    }
    // The narrowing must not have narrowed the rule into never matching.
    expect('card.status === "done"').toMatch(spelledStatus);
    expect('changes: { status: "done" }').toMatch(spelledStatus);
    expect("if (card.status === ARCHIVE_STATUS) return;").toMatch(handRolled);
    expect('row.state === "done"', "an ActionState is not a task status").not.toMatch(spelledStatus);
  });
});
