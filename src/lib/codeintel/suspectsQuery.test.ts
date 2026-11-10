import { get } from "svelte/store";
import { describe, expect, it } from "vitest";
import {
  answerFromPayload,
  bodyChangedLabel,
  canRunSuspects,
  createSuspectsQuery,
  evidenceLabel,
  initialSuspectsState,
  MAX_SUSPECT_CONE_DEPTH,
  relativeScoreWidth,
  suspectMarks,
} from "./suspectsQuery";
import type { CodeintelSuspect, CodeintelSuspectsPayload } from "./types";

function suspect(commit: string, score = 10): CodeintelSuspect {
  return {
    commit,
    author: "ada",
    author_time: 1,
    evidence: "body_changed",
    score,
    nearest_distance: 1,
    touched: [],
  };
}

function payload(
  items: CodeintelSuspect[],
  overrides: Partial<CodeintelSuspectsPayload["response"]> = {},
): CodeintelSuspectsPayload {
  return {
    response: {
      source_freshness: { fresh: true, reason: null },
      available: true,
      items,
      total: items.length,
      shown: items.length,
      truncated: false,
      ...overrides,
    } as CodeintelSuspectsPayload["response"],
    scope: {
      indexed_head: "head",
      since: "HEAD~5",
      cone_size: 7,
      blamed_symbols: 5,
      refusals: [],
    },
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (err: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

describe("answerFromPayload", () => {
  it("never claims fewer rows than it lists", () => {
    const answer = answerFromPayload(payload([suspect("a"), suspect("b")], { total: 1 }), "f", "HEAD~5");
    expect(answer.total).toBe(2);
  });

  it("keeps a refusal apart from an empty walk", () => {
    const refused = answerFromPayload(
      payload([suspect("a")], { available: false, reason: "no index" }),
      "f",
      "HEAD~5",
    );
    expect(refused.available).toBe(false);
    expect(refused.reason).toBe("no index");
    expect(refused.items).toEqual([]);
    expect(refused.total).toBe(0);

    const empty = answerFromPayload(payload([]), "f", "HEAD~5");
    expect(empty.available).toBe(true);
    expect(empty.reason).toBeNull();
    expect(empty.items).toEqual([]);
  });

  it("names a refusal that came with no reason", () => {
    const refused = answerFromPayload(payload([], { available: false, reason: "  " }), "f", "x");
    expect(refused.reason).toBe("the suspects walk did not run");
  });

  it("does not report a refused answer as truncated", () => {
    const refused = answerFromPayload(payload([], { available: false, truncated: true }), "f", "x");
    expect(refused.truncated).toBe(false);
  });
});

describe("canRunSuspects", () => {
  const ready = { ...initialSuspectsState("/r"), symptom: "f" };

  it("needs a repository, a symptom and a base ref", () => {
    expect(canRunSuspects(ready, "/r")).toBe(true);
    expect(canRunSuspects(ready, null)).toBe(false);
    expect(canRunSuspects({ ...ready, symptom: "  " }, "/r")).toBe(false);
    expect(canRunSuspects({ ...ready, since: "" }, "/r")).toBe(false);
    expect(canRunSuspects({ ...ready, loading: true }, "/r")).toBe(false);
  });

  it("refuses a depth the backend would refuse", () => {
    expect(canRunSuspects({ ...ready, depth: 0 }, "/r")).toBe(false);
    expect(canRunSuspects({ ...ready, depth: MAX_SUSPECT_CONE_DEPTH + 1 }, "/r")).toBe(false);
    expect(canRunSuspects({ ...ready, depth: Number.NaN }, "/r")).toBe(false);
    expect(canRunSuspects({ ...ready, depth: 2.5 }, "/r")).toBe(false);
    expect(canRunSuspects({ ...ready, depth: MAX_SUSPECT_CONE_DEPTH }, "/r")).toBe(true);
  });
});

describe("suspectMarks", () => {
  it("ranks in answer order and names the question in the title", () => {
    const state = {
      ...initialSuspectsState("/r"),
      answer: answerFromPayload(payload([suspect("a"), suspect("b")]), "pkg::f", "v1.2"),
    };
    const marks = suspectMarks(state, "/r");
    expect(marks.get("a")?.rank).toBe(1);
    expect(marks.get("b")?.rank).toBe(2);
    expect(marks.get("b")?.title).toContain("pkg::f");
    expect(marks.get("b")?.title).toContain("since v1.2");
  });

  it("marks nothing for another repository, a refusal, or before a run", () => {
    const answered = {
      ...initialSuspectsState("/r"),
      answer: answerFromPayload(payload([suspect("a")]), "f", "x"),
    };
    expect(suspectMarks(answered, "/other").size).toBe(0);
    expect(suspectMarks(answered, null).size).toBe(0);
    expect(suspectMarks(initialSuspectsState("/r"), "/r").size).toBe(0);
    const refused = {
      ...answered,
      answer: answerFromPayload(payload([suspect("a")], { available: false }), "f", "x"),
    };
    expect(suspectMarks(refused, "/r").size).toBe(0);
  });
});

describe("rendering helpers", () => {
  it("scales against the top hit and never divides by zero", () => {
    expect(relativeScoreWidth(5, 10)).toBe(50);
    expect(relativeScoreWidth(0, 0)).toBe(0);
  });

  it("keeps three states and passes unknown evidence through", () => {
    expect(bodyChangedLabel(true)).toBe("changed");
    expect(bodyChangedLabel(false)).toBe("moved");
    expect(bodyChangedLabel(null)).toBe("could not tell");
    expect(evidenceLabel("new_class")).toBe("new_class");
  });
});

describe("createSuspectsQuery", () => {
  it("runs the question as asked and keeps the answer across reads", async () => {
    const calls: unknown[][] = [];
    const q = createSuspectsQuery({
      fetch: async (...args) => {
        calls.push(args);
        return payload([suspect("a")]);
      },
    });
    q.bindRepo("/r");
    q.setInputs({ symptom: "  pkg::f  ", since: "v1", depth: 4 });
    await q.run("/r");
    expect(calls).toEqual([["/r", "pkg::f", "v1", 4]]);
    const state = get(q);
    expect(state.loading).toBe(false);
    expect(state.answer?.items.map((s) => s.commit)).toEqual(["a"]);
    expect(state.answer?.symptom).toBe("pkg::f");
  });

  it("does not run without a symptom", async () => {
    let called = false;
    const q = createSuspectsQuery({ fetch: async () => ((called = true), payload([])) });
    await q.run("/r");
    expect(called).toBe(false);
    expect(get(q).answer).toBeNull();
  });

  it("drops the answer and the inputs when the repository changes", async () => {
    const q = createSuspectsQuery({ fetch: async () => payload([suspect("a")]) });
    q.bindRepo("/r");
    q.setInputs({ symptom: "f" });
    await q.run("/r");
    q.bindRepo("/other");
    const state = get(q);
    expect(state.repoPath).toBe("/other");
    expect(state.answer).toBeNull();
    expect(state.symptom).toBe("");
  });

  it("lets a late answer for a previous repository land nowhere", async () => {
    const pending = deferred<CodeintelSuspectsPayload>();
    const q = createSuspectsQuery({ fetch: () => pending.promise });
    q.bindRepo("/r");
    q.setInputs({ symptom: "f" });
    const run = q.run("/r");
    q.bindRepo("/other");
    pending.resolve(payload([suspect("a")]));
    await run;
    expect(get(q).answer).toBeNull();
    expect(get(q).loading).toBe(false);
  });

  it("lets only the newest of two runs land", async () => {
    const first = deferred<CodeintelSuspectsPayload>();
    const second = deferred<CodeintelSuspectsPayload>();
    const queue = [first.promise, second.promise];
    const q = createSuspectsQuery({ fetch: () => queue.shift()! });
    q.bindRepo("/r");
    q.setInputs({ symptom: "f" });
    const a = q.run("/r");
    // A re-seed from the details pane asks again while the first is loading.
    const b = q.seedSince("/r", "abc");
    second.resolve(payload([suspect("second")]));
    await b;
    first.resolve(payload([suspect("first")]));
    await a;
    expect(get(q).answer?.items.map((s) => s.commit)).toEqual(["second"]);
  });

  it("treats a thrown error as no answer, not as an empty one", async () => {
    const q = createSuspectsQuery({
      fetch: async () => {
        throw new Error("ipc down");
      },
      onError: (err) => String((err as Error).message),
    });
    q.bindRepo("/r");
    q.setInputs({ symptom: "f" });
    await q.run("/r");
    const state = get(q);
    expect(state.error).toBe("ipc down");
    expect(state.answer).toBeNull();
    expect(state.loading).toBe(false);
  });

  it("clears the last answer's marks when a run fails", async () => {
    let fail = false;
    const q = createSuspectsQuery({
      fetch: async () => {
        if (fail) throw new Error("x");
        return payload([suspect("a")]);
      },
      onError: () => "x",
    });
    q.bindRepo("/r");
    q.setInputs({ symptom: "f" });
    await q.run("/r");
    expect(suspectMarks(get(q), "/r").size).toBe(1);
    fail = true;
    await q.run("/r");
    expect(suspectMarks(get(q), "/r").size).toBe(0);
  });

  it("keeps a focus request across the first bind", () => {
    // The palette door asks before History has mounted and bound the repo.
    const q = createSuspectsQuery({ fetch: async () => payload([]) });
    q.requestFocus();
    q.bindRepo("/r");
    expect(get(q).focusPending).toBe(true);
  });

  it("seeds the window from a commit and runs when a symptom is named", async () => {
    const calls: unknown[][] = [];
    const q = createSuspectsQuery({
      fetch: async (...args) => {
        calls.push(args);
        return payload([]);
      },
    });
    q.bindRepo("/r");
    await q.seedSince("/r", "abc123");
    expect(get(q).since).toBe("abc123");
    expect(calls).toEqual([]);
    expect(get(q).focusPending).toBe(true);
    q.consumeFocus();
    expect(get(q).focusPending).toBe(false);

    q.setInputs({ symptom: "f" });
    await q.seedSince("/r", "def456");
    expect(calls).toEqual([["/r", "f", "def456", 3]]);
    expect(get(q).focusPending).toBe(false);
  });
});
