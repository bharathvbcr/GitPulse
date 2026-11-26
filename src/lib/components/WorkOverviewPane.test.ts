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
  isBare: false,
  retained: 0,
  verdict: null as null | "blocked" | "allowed",
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
            isBare: state.isBare,
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

vi.mock("../stores/harnessStore", async (importOriginal) => {
  const real = await importOriginal<typeof import("../stores/harnessStore")>();
  return {
    ...real,
    harnessStore: {
      ...real.harnessStore,
      subscribe: (run: (value: unknown) => void) =>
        real.harnessStore.subscribe((value) =>
          run({ ...value, lastVerdict: state.verdict ? { ...(value.lastVerdict ?? {}), status: state.verdict } : null }),
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
  Object.assign(state, { conflicted: false, parked: false, probeFailed: false, isBare: false, retained: 0, verdict: null });
});

describe("WorkOverviewPane", () => {
  it("offers one door to the blocked verdict's explanation, above the rows", () => {
    state.verdict = "blocked";
    const body = pane();
    expect(body).toContain('data-testid="work-blocked-verdict"');
    expect(body).toContain("Manvi blocked the last action.");
    expect(body).toContain("Show activity");
    expect(body.indexOf("work-blocked-verdict")).toBeLessThan(body.indexOf(OVERVIEW));
  });

  it("drops the verdict notice once the verdict changes, and while the editor leads", () => {
    state.verdict = "allowed";
    expect(pane()).not.toContain("work-blocked-verdict");
    Object.assign(state, { verdict: "blocked", conflicted: true });
    const body = pane();
    expect(body).toContain(EDITOR);
    expect(body).not.toContain("work-blocked-verdict");
  });

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
    // The buttons reach the anchor through the one door, which the pane
    // answers by scrolling to this id (ui/workConflicts.test.ts).
    expect(view).toContain("openConflicts();");
    const own = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "WorkOverviewPane.svelte"), "utf8");
    expect(own).toContain("document.getElementById(WORK_CONFLICTS_ID)");
  });

  it("exposes accessible Conflicts and Resolve triggers in Overview even when clean or draft-only", () => {
    const clean = pane();
    expect(clean).toContain('data-testid="overview-conflicts-btn"');
    expect(clean).toContain('data-testid="overview-repo-conflicts-btn"');
    expect(clean).not.toContain('data-testid="work-conflicts-bar"');

    state.retained = 2;
    const withDrafts = pane();
    expect(withDrafts).toContain('data-testid="overview-conflicts-btn"');
    expect(withDrafts).toContain('data-testid="overview-repo-conflicts-btn"');
    expect(withDrafts).toContain('Conflicts (2 saved drafts)');
  });

  it("renders the pane navigation bar above the scroller without Hide while live", () => {
    for (const setup of [{ conflicted: true }, { parked: true }, { probeFailed: true }] as const) {
      Object.assign(state, { conflicted: false, parked: false, probeFailed: false, ...setup });
      const body = pane();
      const barStart = body.indexOf('data-testid="work-conflicts-bar"');
      const scrollerStart = body.indexOf('data-overview-layout="with-conflicts"');
      expect(barStart).toBeGreaterThan(-1);
      expect(scrollerStart).toBeGreaterThan(barStart);
      const bar = body.slice(barStart, scrollerStart);
      expect(bar).toContain("Conflicts");
      expect(bar).toContain("Overview");
      expect(bar).not.toContain("Hide");
    }
  });

  it("stays on Overview without draft rows, editor, or Resolve triggers on bare repositories with retained drafts", () => {
    Object.assign(state, { isBare: true, retained: 3 });
    const body = pane();
    expect(body).toContain('data-overview-layout="overview"');
    expect(body).not.toContain('data-overview-layout="with-conflicts"');
    expect(body).not.toContain(DRAFTS);
    expect(body).not.toContain('id="work-conflicts"');
    expect(body).not.toContain('data-testid="overview-repo-conflicts-btn"');
  });

  it("stays on Overview without pinning the editor or Resolve buttons when bare and probeFailed", () => {
    Object.assign(state, { isBare: true, probeFailed: true });
    const body = pane();
    expect(body).toContain('data-overview-layout="overview"');
    expect(body).not.toContain('data-overview-layout="with-conflicts"');
    expect(body).not.toContain('id="work-conflicts"');
    expect(body).not.toContain('data-testid="overview-repo-conflicts-btn"');
    expect(body).not.toContain('data-testid="overview-probe-failed-conflicts-btn"');
    expect(body).toContain("Couldn't check for a parked merge or rebase here");
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

  it("handles reveal requests with bare-repo guard, clears manual reveal on repo switch/live/bare, and refreshes retained drafts immediately", () => {
    const effect = source.slice(source.indexOf("if (!$conflictsFocusRequest) return;"));
    expect(effect).toContain("const reveal = takeConflictsReveal();");
    expect(effect).toContain("if (reveal && !live && !isBare) manualOpen = true;");
    expect(effect.indexOf("if (reveal && !live && !isBare) manualOpen = true;")).toBeLessThan(effect.indexOf("if (!showEditor) return;"));

    expect(source).toContain("setConflictsPanelOpen(showEditor);");
    expect(source).toContain("if (prevRepo !== undefined && repo !== prevRepo)");
    expect(source).toContain("if (live || isBare)");
    expect(source).toContain("void $conflictDraftProbe;");
    expect(source).toContain("bumpConflictDraftProbe();");
    expect(source).toContain("const showEditor = $derived(!isBare && (live || manualOpen));");
    expect(source).toContain("focusConflictsSection();");
    expect(source).toContain('[data-testid="overview-conflicts-btn"]');
    expect(source).toContain("overviewSection?.focus({ preventScroll: true });");
    const barTag = source.slice(source.lastIndexOf("<div", source.indexOf('data-testid="work-conflicts-bar"')), source.indexOf(">", source.indexOf('data-testid="work-conflicts-bar"')) + 1);
    expect(barTag).not.toMatch(/\bbg-surface(Hover)?\b/);
  });
});
