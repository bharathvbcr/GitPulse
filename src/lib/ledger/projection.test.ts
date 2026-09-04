import { describe, expect, it } from "vitest";
import {
  eventToAction,
  isGateRow,
  kindForAction,
  labelFor,
  mergeEvents,
  verdictOf,
} from "./projection";
import type { LedgerEvent } from "./types";
import type { AgentActionEntry } from "../agents/activity";

function event(overrides: Partial<LedgerEvent> = {}): LedgerEvent {
  return {
    id: 1,
    ulid: "01M1F8Q43R3S5XG2A200000005",
    ts_utc: "2026-09-01T12:00:00.000Z",
    schema_version: 1,
    repo_path: "/repo",
    worktree_path: null,
    actor_kind: "human",
    actor_id: null,
    session_id: null,
    task_id: null,
    action: "git.commit",
    object: "git commit -m x",
    argv_json: '["git","commit","-m","x"]',
    outcome: "ok",
    verdict_json: null,
    before_ref: null,
    after_ref: null,
    duration_ms: null,
    detail_json: null,
    ...overrides,
  };
}

describe("kindForAction", () => {
  it("maps the git verbs the journal already renders", () => {
    expect(kindForAction("git.commit")).toBe("commit");
    expect(kindForAction("git.push")).toBe("push");
    expect(kindForAction("git.rebase")).toBe("rebase");
    expect(kindForAction("git.cherry_pick")).toBe("cherry-pick");
  });

  it("renders any file operation as an edit", () => {
    expect(kindForAction("file.modify")).toBe("edit");
    expect(kindForAction("file.create")).toBe("edit");
  });

  it("passes an unmapped verb through rather than dropping it", () => {
    // The journal is the record of what an unattended agent did. A verb this
    // build does not know is still something that happened.
    expect(kindForAction("git.bisect")).toBe("bisect");
    expect(kindForAction("command.run")).toBe("run");
  });

  it("survives a malformed action name", () => {
    expect(kindForAction("")).toBe("action");
    expect(kindForAction("nodot")).toBe("nodot");
  });
});

describe("verdictOf", () => {
  it("parses a verdict the row carries", () => {
    const v = verdictOf(event({ verdict_json: '{"status":"blocked","rule":"path.secret"}' }));
    expect(v?.status).toBe("blocked");
  });

  it("returns null for a missing verdict rather than inventing one", () => {
    expect(verdictOf(event({ verdict_json: null }))).toBeNull();
  });

  it("returns null for an unreadable verdict", () => {
    // A verdict we cannot parse is not a verdict that passed. Returning null
    // renders it as "no policy decision", which is the honest reading.
    expect(verdictOf(event({ verdict_json: "{not json" }))).toBeNull();
  });
});

describe("isGateRow", () => {
  it("recognises a gate decision", () => {
    expect(isGateRow(event({ detail_json: '{"phase":"gate"}' }))).toBe(true);
  });

  it("does not treat a completion row as a gate row", () => {
    expect(isGateRow(event({ detail_json: '{"files_changed":3}' }))).toBe(false);
    expect(isGateRow(event({ detail_json: null }))).toBe(false);
  });

  it("does not guess when the detail is unreadable", () => {
    expect(isGateRow(event({ detail_json: "{oops" }))).toBe(false);
  });
});

describe("eventToAction", () => {
  it("projects a successful action", () => {
    const a = eventToAction(event());
    expect(a).toMatchObject({ id: 1, kind: "commit", label: "git commit -m x", ok: true });
    expect(a.ts).toBe(Date.parse("2026-09-01T12:00:00.000Z"));
  });

  it("marks a blocked action as not ok", () => {
    expect(eventToAction(event({ outcome: "blocked" })).ok).toBe(false);
  });

  it("marks a failed action as not ok", () => {
    expect(eventToAction(event({ outcome: "failed" })).ok).toBe(false);
  });

  it("falls back to the action name when there is no object", () => {
    expect(labelFor(event({ object: null }))).toBe("git.commit");
  });

  it("includes both canonical repository and ULID in durable identity", () => {
    const base = eventToAction(
      event({ id: 7, repo_path: "/repo/a", ulid: "01DURABLE000000000000000007" }),
    );
    const otherRepository = eventToAction(
      event({ id: 7, repo_path: "/repo/b", ulid: "01DURABLE000000000000000007" }),
    );
    const otherUlid = eventToAction(
      event({ id: 7, repo_path: "/repo/a", ulid: "01DURABLE000000000000000008" }),
    );

    expect(base.identity).not.toBe(otherRepository.identity);
    expect(base.identity).not.toBe(otherUlid.identity);
  });
});

describe("mergeEvents", () => {
  it("appends new events in id order", () => {
    const merged = mergeEvents([], [event({ id: 2 }), event({ id: 1 })], 100);
    expect(merged.map((e) => e.id)).toEqual([1, 2]);
  });

  it("ignores rows already projected", () => {
    // A notification and a poll can race and re-deliver the same row.
    const first = mergeEvents([], [event({ id: 1 })], 100);
    const again = mergeEvents(first, [event({ id: 1 }), event({ id: 2 })], 100);
    expect(again.map((e) => e.id)).toEqual([1, 2]);
  });

  it("returns the same list when there is nothing new", () => {
    const first = mergeEvents([], [event({ id: 1 })], 100);
    expect(mergeEvents(first, [], 100)).toBe(first);
  });

  it("caps the view without pretending the older rows never happened", () => {
    // The cap is a *display* limit now. The rows past it are still on disk;
    // this only bounds what is held for rendering.
    const events = Array.from({ length: 10 }, (_, i) => event({ id: i + 1 }));
    const merged = mergeEvents([], events, 4);
    expect(merged).toHaveLength(4);
    expect(merged.map((e) => e.id)).toEqual([7, 8, 9, 10]);
  });

  it("keeps the newest rows when the cap is reached across calls", () => {
    let list = mergeEvents([], [event({ id: 1 }), event({ id: 2 })], 3);
    list = mergeEvents(list, [event({ id: 3 }), event({ id: 4 })], 3);
    expect(list.map((e) => e.id)).toEqual([2, 3, 4]);
  });

  it("orders mixed ephemeral and durable identities by time, not unrelated numeric ids", () => {
    const ephemeral: AgentActionEntry = {
      identity: '["ephemeral","/repo",1]',
      id: 1,
      ts: Date.parse("2026-09-01T13:00:00Z"),
      kind: "commit",
      label: "new ephemeral action",
      ok: true,
      verdict: null,
    };

    const merged = mergeEvents(
      [ephemeral],
      [
        event({
          id: 100,
          ulid: "01M1F8Q43R3S5XG2A200000100",
          ts_utc: "2026-09-01T12:00:00Z",
          object: "older durable action",
        }),
      ],
      100,
    );

    expect(merged.map((action) => action.label)).toEqual([
      "older durable action",
      "new ephemeral action",
    ]);
  });
});
