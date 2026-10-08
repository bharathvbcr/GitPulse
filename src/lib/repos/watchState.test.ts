import { describe, expect, it } from "vitest";
import {
  WATCH_ACTIVE,
  WATCH_UNKNOWN,
  describeWatch,
  isLiveUpdating,
  needsFullPoll,
  shouldSurface,
  watchFailed,
  watchMarker,
  watchStatesEqual,
  type WatchState,
} from "./watchState";

const ALL: WatchState[] = [
  WATCH_ACTIVE,
  WATCH_UNKNOWN,
  watchFailed("Too many watched repositories (max 24)"),
];

describe("watchFailed", () => {
  it("keeps the backend's reason, which names the real limit", () => {
    const state = watchFailed(new Error("Too many watched repositories (max 24)"));
    expect(state.status).toBe("degraded");
    expect(state.reason).toBe("Too many watched repositories (max 24)");
  });

  it("accepts a bare string reason", () => {
    expect(watchFailed("inotify limit reached").reason).toBe("inotify limit reached");
  });

  it("never produces a degraded state with no reason at all", () => {
    for (const input of [null, undefined, {}, "", "   ", 0]) {
      const state = watchFailed(input);
      expect(state.status).toBe("degraded");
      expect(state.reason, JSON.stringify(input)).toBeTruthy();
    }
  });
});

describe("isLiveUpdating", () => {
  it("is true only for a confirmed watch", () => {
    expect(isLiveUpdating(WATCH_ACTIVE)).toBe(true);
    expect(isLiveUpdating(WATCH_UNKNOWN)).toBe(false);
    expect(isLiveUpdating(watchFailed("nope"))).toBe(false);
  });
});

describe("needsFullPoll", () => {
  it("compensates for a degraded watch", () => {
    // Without this the indicator would announce staleness and do nothing
    // about it: branches, the graph, the operation banner and the stash are
    // refreshed by the watcher and by nothing else on an open tab.
    expect(needsFullPoll(watchFailed("full"))).toBe(true);
  });

  it("compensates while the watch state is still unknown", () => {
    // Assuming live updates before the attempt settles is the assumption that
    // leaves a user staring at stale data; one extra snapshot is the cost of
    // being wrong the other way.
    expect(needsFullPoll(WATCH_UNKNOWN)).toBe(true);
  });

  it("does not pay for a full refresh when the watcher is working", () => {
    expect(needsFullPoll(WATCH_ACTIVE)).toBe(false);
  });
});

describe("shouldSurface", () => {
  it("tells the user only when updates are genuinely degraded", () => {
    expect(shouldSurface(watchFailed("full"))).toBe(true);
  });

  it("stays silent while the watch is still being set up", () => {
    // An indicator that flickers on every repository open trains people to
    // ignore the one time it matters.
    expect(shouldSurface(WATCH_UNKNOWN)).toBe(false);
    expect(watchMarker(WATCH_UNKNOWN)).toBeNull();
  });

  it("stays silent when everything is working", () => {
    expect(shouldSurface(WATCH_ACTIVE)).toBe(false);
    expect(watchMarker(WATCH_ACTIVE)).toBeNull();
  });

  it("keeps the marker short enough for a dense status-bar row", () => {
    const marker = watchMarker(watchFailed("full"));
    expect(marker!.length).toBeLessThanOrEqual(12);
  });

  it("renders a short marker when it does speak", () => {
    expect(watchMarker(watchFailed("full"))).toBe("Not live");
  });
});

describe("describeWatch", () => {
  it("produces a usable sentence for every state", () => {
    for (const state of ALL) {
      const text = describeWatch(state);
      expect(text, state.status).toBeTruthy();
      expect(text).not.toContain("undefined");
      expect(text).not.toContain("null");
    }
  });

  it("says what is wrong, what the app is doing, and what it costs", () => {
    // An indicator that only announces a problem leaves the reader nowhere
    // to go.
    const text = describeWatch(watchFailed("Too many watched repositories (max 24)"));
    expect(text).toContain("Too many watched repositories (max 24)");
    expect(text).toContain("refreshing on a timer");
    expect(text).toContain("outside GitPulse");
  });

  it("reads sensibly with no reason recorded", () => {
    const text = describeWatch({ status: "degraded", reason: null });
    expect(text).toContain("not receiving live filesystem updates");
    expect(text).not.toContain("()");
  });
});

describe("watchStatesEqual", () => {
  it("treats rebuilt-but-identical states as unchanged", () => {
    // The state is rebuilt on every open and refresh; reference equality
    // would republish the store to every subscriber each time.
    expect(watchStatesEqual(watchFailed("full"), watchFailed("full"))).toBe(true);
    expect(watchStatesEqual(WATCH_ACTIVE, { status: "watching", reason: null })).toBe(true);
  });

  it("notices a change of status or of reason", () => {
    expect(watchStatesEqual(WATCH_ACTIVE, WATCH_UNKNOWN)).toBe(false);
    expect(watchStatesEqual(watchFailed("a"), watchFailed("b"))).toBe(false);
  });

  it("is reflexive", () => {
    for (const state of ALL) {
      expect(watchStatesEqual(state, state)).toBe(true);
    }
  });
});
