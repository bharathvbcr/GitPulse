import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

/**
 * `docs/ARCHIVE_SEPARATION.md` records how a task's archived flag became
 * independent of its status (dc-store schema 11). Every claim it makes about
 * the vendored store is a claim about a file in this repository, and a record
 * whose premise has quietly stopped being true misdirects the next reader as
 * badly as a stale plan did. So this pins them.
 *
 * It also pins the seam from the other side: one module decides what
 * "archived" means, and no surface compares a status or the raw flag itself.
 */
const url = (path: string) => new URL(`../${path}`, import.meta.url);
const read = (path: string) => readFileSync(url(path), "utf8");

const STORE = "src-tauri/vendored/dc-store/src/workbench/mod.rs";
const MIGRATION = "src-tauri/vendored/dc-store/src/workbench/items.sql";
const record = read("docs/ARCHIVE_SEPARATION.md");
const store = read(STORE);
const migration = read(MIGRATION);

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

describe("the store the record describes", () => {
  it("stores the flag and the completion time as generated columns over the body", () => {
    expect(migration).toMatch(/ALTER TABLE work_items ADD COLUMN archived INTEGER\s+GENERATED ALWAYS AS \(coalesce\(json_extract\(body,'\$\.archived'\),0\)\) VIRTUAL;/);
    expect(migration).toMatch(/ALTER TABLE work_items ADD COLUMN completed_at INTEGER\s+GENERATED ALWAYS AS \(coalesce\(json_extract\(body,'\$\.completed_at'\),0\)\) VIRTUAL;/);
    expect(record).toContain("VIRTUAL");
  });

  it("migrates every Done task into the archive, without spending a revision", () => {
    expect(migration).toContain("'$.archived',json(CASE WHEN status='done' THEN 'true' ELSE 'false' END)");
    expect(migration).not.toMatch(/SET\s+revision/);
    expect(record).toContain("migrates every Done task to archived");
  });

  // Every row, not only Done ones, so a task written before the upgrade reads
  // in the same shape as one written after.
  it("gives every row the new keys, as a schema 11 write would", () => {
    const update = migration.slice(migration.indexOf("UPDATE work_items SET body=json_set(body,"), migration.indexOf("UPDATE work_meta"));
    expect(update).not.toMatch(/\bWHERE status='done';/);
    for (const key of ["$.archived", "$.completed_at", "$.checklist", "$.links"]) expect(update).toContain(`'${key}'`);
    expect(record).toContain("gives **every other row** the same keys");
  });

  it("writes the new fields and never accepts the store-owned completion time", () => {
    const fields = acceptedFields(store, "put_item");
    for (const field of ["archived", "checklist", "links"]) expect(fields).toContain(field);
    expect(fields).not.toContain("completed_at");
    for (const field of ["archived", "checklist", "links"]) expect(record).toContain(`\`${field}\``);
  });

  it("filters and orders a list the way the record says", () => {
    const fields = acceptedFields(store, "list_items");
    expect(fields).toEqual(["limit", "cursor", "workspace_id", "repository_id", "status", "query", "archived", "order", "deleted"]);
    for (const order of ["board", "completed", "updated"]) {
      expect(store).toContain(`Some("${order}")`);
      expect(record).toContain(`\`${order}\``);
    }
  });

  it("names the schema version the store ends its ladder on", () => {
    const terminal = /if version != (\d+) \{/.exec(store)?.[1];
    expect(terminal, "the migration ladder must end in a supported-version check").toBe("11");
    expect(record).toContain(`Schema version is **${terminal}**.`);
  });

  it("names the vendored commit it was written against", () => {
    const vendor = JSON.parse(read("src-tauri/vendored/VENDOR.json"));
    const dcStore = vendor.crates.find((crate: { name: string }) => crate.name === "dc-store");
    expect(dcStore, "VENDOR.json must describe dc-store").toBeDefined();
    expect(record).toContain(dcStore.origin.commit.slice(0, 7));
  });
});

describe("the seam", () => {
  const archive = read("src/lib/workbench/taskArchive.ts");

  it("keeps every function the record names", () => {
    for (const owner of ["isArchived", "archiveAction", "restoreAction", "ARCHIVE_RULE", "refreshPages"]) {
      expect(archive, `taskArchive.ts must export ${owner}`).toMatch(new RegExp(`export (?:async )?(?:const|function) ${owner}\\b`));
      expect(record, `the record must account for ${owner}`).toContain(owner);
    }
    // Retired with the coupling to the Done column; the record says so.
    for (const gone of ["boardPresence", "offersArchive", "ARCHIVE_STATUS"]) {
      expect(archive).not.toContain(`export const ${gone}`);
      expect(archive).not.toContain(`export function ${gone}`);
    }
  });

  /**
   * The whole value of the seam. One module decides what archived means; a
   * surface that compares a status to Done, or reads the raw flag, is a second
   * decision the next change to the rule would not reach.
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
     * The decision `isArchived` owns, made from a task's raw field. Workspaces
     * have an `archived` flag of their own (`group.archived`), which is a
     * different thing and not this module's.
     */
    const rawFlag = /\b(?:card|task|item|row|entry|full|saved)s?\w*\.archived\b/i;
    /** Archived spelled as a status. */
    const statusArchive = /status\s*[!=]==?\s*["']done["'][^\n]*archiv|archiv[^\n]*status\s*[!=]==?\s*["']done["']/i;
    for (const path of surfaces) {
      const source = read(path);
      expect(source, `${path} must ask isArchived, not read .archived`).not.toMatch(rawFlag);
      expect(source, `${path} must not equate archived with Done`).not.toMatch(statusArchive);
    }
    // The rule must still be able to match what it forbids.
    expect("if (card.archived === true) return;").toMatch(rawFlag);
    expect("const hidden = !card.archived;").toMatch(rawFlag);
    expect("archived = card.status === 'done'").toMatch(statusArchive);
    expect("archived: false").not.toMatch(rawFlag);
    expect("workspaces.filter((group) => !group.archived)").not.toMatch(rawFlag);
  });
});
