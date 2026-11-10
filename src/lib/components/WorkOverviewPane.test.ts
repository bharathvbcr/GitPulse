/**
 * Work → Overview with the conflict editor folded in: the editor leads the
 * pane exactly when it has something to show, and Overview is always there.
 *
 * Rendered on the server, so the lazy conflict-editor chunk never loads and
 * its LazyView skeleton, whose accessible name says which pane it is, stands in
 * for it. Overview itself renders for real.
 */
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, describe, expect, it, vi } from "vitest";
import { render } from "svelte/server";
import type { RepoState } from "../stores/repoStore";
import WorkOverviewPane from "./WorkOverviewPane.svelte";

const state = vi.hoisted(() => ({
  conflicted: false,
  parked: false,
  probeFailed: false,
  retained: 0,
}));

vi.mock("../stores/repoStore", async (importOriginal) => {
  const real = await importOriginal<typeof import("../stores/repoStore")>();
  // The pane reads the repository through field stores, so the mock has to
  // answer them as well as the whole-store subscription.
  const fieldStore = <T,>(read: () => T) => ({
    subscribe: (run: (value: T) => void) => {
      run(read());
      return () => {};
    },
  });
  const conflictedStatuses = () =>
    state.conflicted
      ? [{ ...({} as RepoState["statuses"][number]), path: "a.txt", is_conflicted: true }]
      : [];
  return {
    ...real,
    currentRepoPath: fieldStore(() => "/repo"),
    repoStore: {
      ...real.repoStore,
      currentPath: fieldStore(() => "/repo"),
      statuses: fieldStore(conflictedStatuses),
      conflictedCount: fieldStore(() => (state.conflicted ? 1 : 0)),
      subscribe: (run: (value: RepoState) => void) =>
        real.repoStore.subscribe((value) =>
          run({
            ...value,
            currentPath: "/repo",
            statuses: conflictedStatuses(),
            conflictedCount: state.conflicted ? 1 : 0,
            operation: {
              probeFailed: state.probeFailed,
              operation: state.parked
                ? {
                    kind: "Merge",
                    current_step: null,
                    total_steps: null,
                    head_ref: null,
                    incoming_ref: null,
                    conflicted_paths: [],
                    conflicted_total: 0,
                    available: [],
                  }
                : null,
            },
          } as RepoState),
        ),
    },
  };
});

vi.mock("../diff/conflictSession", async (importOriginal) => {
  const real = await importOriginal<typeof import("../diff/conflictSession")>();
  return {
    ...real,
    conflictSessions: {
      ...real.conflictSessions,
      summaries: (repo: string) =>
        repo === "/repo"
          ? Array.from({ length: state.retained }, (_, i) => ({ file: `kept-${i}.txt` }))
          : [],
    },
  };
});

const never = () => new Promise<never>(() => {});
const pane = () => render(WorkOverviewPane, { props: { loadConflict: never } }).body;
const EDITOR = 'aria-label="Loading the conflict editor"';
const OVERVIEW = 'aria-label="Overview"';
const DRAFTS = 'data-testid="work-retained-drafts"';

afterEach(() => {
  Object.assign(state, { conflicted: false, parked: false, probeFailed: false, retained: 0 });
});

describe("WorkOverviewPane", () => {
  it("is Overview alone when there is nothing to resolve", () => {
    const body = pane();
    expect(body).toContain(OVERVIEW);
    expect(body).not.toContain(EDITOR);
    expect(body).not.toContain('data-overview-layout="with-conflicts"');
  });

  it.each([
    ["a file carries conflict markers", { conflicted: true }],
    // Every file resolved but the merge not continued: the editor's staged
    // review and continue banner are the only way forward.
    ["a merge is parked with nothing left conflicted", { parked: true }],
    // An unreadable operation must be said, not hidden behind Overview.
    ["the operation probe failed", { probeFailed: true }],
    // Live conflicts lead even when stale drafts are kept as well.
    ["conflicts stand beside retained drafts", { conflicted: true, retained: 2 }],
  ] as const)("leads with the conflict editor when %s, with Overview below", (_, setup) => {
    Object.assign(state, setup);
    const body = pane();
    expect(body).toContain('data-overview-layout="with-conflicts"');
    expect(body).toContain(EDITOR);
    expect(body).toContain(OVERVIEW);
    expect(body.indexOf(EDITOR)).toBeLessThan(body.indexOf(OVERVIEW));
    expect(body).not.toContain(DRAFTS);
  });

  it("gives drafts kept from an earlier operation a row, not the whole pane", () => {
    // Drafts never expire. One stale draft must not pin a full-height editor
    // over Overview, and it must not lose its only door either.
    state.retained = 2;
    const body = pane();
    expect(body).toContain(DRAFTS);
    expect(body).toContain("2 conflict drafts kept from an earlier merge or rebase.");
    expect(body).toContain("Review drafts");
    expect(body).not.toContain(EDITOR);
    expect(body).toContain(OVERVIEW);
    expect(body).not.toContain('data-overview-layout="with-conflicts"');
  });

  it("anchors the editor where the Resolve buttons scroll to", () => {
    Object.assign(state, { conflicted: true });
    expect(pane()).toContain('id="work-conflicts"');
    const view = readFileSync(
      join(dirname(fileURLToPath(import.meta.url)), "WorkView.svelte"),
      "utf8",
    );
    expect(view).toContain("WORK_CONFLICTS_ID");
  });
});

describe("WorkOverviewPane source", () => {
  const source = readFileSync(
    join(dirname(fileURLToPath(import.meta.url)), "WorkOverviewPane.svelte"),
    "utf8",
  );

  it("mounts Overview once, outside every branch, so a merge that parks cannot rebuild it", () => {
    expect(source.match(/<WorkView \/>/g)).toHaveLength(1);
    const outsideBranches = source.slice(source.lastIndexOf("{/if}"));
    expect(outsideBranches).toContain("<WorkView />");
  });

  it("reads the repository through field stores, never by walking the whole store", () => {
    // `$repoStore.statuses.some(...)` re-runs on every status publish and is
    // what repoStoreEffects.contract.test.ts forbids.
    expect(source).not.toMatch(/\$repoStore\.statuses/);
    expect(source).toContain("const { conflictedCount, statuses } = repoStore;");
    expect(source).toContain("$conflictedCount > 0");
  });
});
