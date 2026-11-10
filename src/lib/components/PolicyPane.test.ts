/**
 * Work → Policy after Resolve folded into it: the conflict editor leads the
 * pane exactly when it has something to show, and MANVI is always there.
 *
 * Rendered on the server, so neither lazy chunk loads and each pane shows its
 * LazyView skeleton, whose accessible name says which pane it is. That is
 * enough to tell which panes are on screen without loading either one.
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { render } from "svelte/server";
import type { RepoState } from "../stores/repoStore";
import PolicyPane from "./PolicyPane.svelte";

const state = vi.hoisted(() => ({
  conflicted: false,
  parked: false,
  probeFailed: false,
  retained: 0,
}));

vi.mock("../stores/repoStore", async (importOriginal) => {
  const real = await importOriginal<typeof import("../stores/repoStore")>();
  return {
    ...real,
    repoStore: {
      ...real.repoStore,
      subscribe: (run: (value: RepoState) => void) =>
        real.repoStore.subscribe((value) =>
          run({
            ...value,
            currentPath: "/repo",
            statuses: state.conflicted
              ? [{ ...({} as RepoState["statuses"][number]), path: "a.txt", is_conflicted: true }]
              : [],
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
const pane = () => render(PolicyPane, { props: { loadConflict: never, loadManvi: never } }).body;
const EDITOR = 'aria-label="Loading the conflict editor"';
const MANVI = 'aria-label="Loading Manvi ops"';
const DRAFTS = 'data-testid="policy-retained-drafts"';

afterEach(() => {
  Object.assign(state, { conflicted: false, parked: false, probeFailed: false, retained: 0 });
});

describe("PolicyPane", () => {
  it("is MANVI alone when there is nothing to resolve", () => {
    const body = pane();
    expect(body).toContain(MANVI);
    expect(body).not.toContain(EDITOR);
    expect(body).not.toContain('data-policy-layout="with-conflicts"');
  });

  it.each([
    ["a file carries conflict markers", { conflicted: true }],
    // Every file resolved but the merge not continued: the editor's staged
    // review and continue banner are the only way forward.
    ["a merge is parked with nothing left conflicted", { parked: true }],
    // An unreadable operation must be said, not hidden behind MANVI.
    ["the operation probe failed", { probeFailed: true }],
    // Live conflicts lead even when stale drafts are kept as well.
    ["conflicts stand beside retained drafts", { conflicted: true, retained: 2 }],
  ] as const)("leads with the conflict editor when %s, with MANVI below", (_, setup) => {
    Object.assign(state, setup);
    const body = pane();
    expect(body).toContain('data-policy-layout="with-conflicts"');
    expect(body).toContain(EDITOR);
    expect(body).toContain(MANVI);
    expect(body.indexOf(EDITOR)).toBeLessThan(body.indexOf(MANVI));
    expect(body).not.toContain(DRAFTS);
  });

  it("gives drafts kept from an earlier operation a row, not the whole pane", () => {
    // Drafts never expire. One stale draft must not pin a full-height editor
    // over MANVI, and it must not lose its only door either.
    state.retained = 2;
    const body = pane();
    expect(body).toContain(DRAFTS);
    expect(body).toContain("2 conflict drafts kept from an earlier merge or rebase.");
    expect(body).toContain("Review drafts");
    expect(body).not.toContain(EDITOR);
    expect(body).toContain(MANVI);
    expect(body).not.toContain('data-policy-layout="with-conflicts"');
  });
});
