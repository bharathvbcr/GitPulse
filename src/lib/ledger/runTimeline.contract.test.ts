import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { SESSION_STATE_EVENTS, stateFor } from "./runTimeline";

/**
 * The lifecycle vocabulary is written twice: Rust records a `session.state`
 * row (`ledger/lifecycle.rs`) and the timeline reads it back. A renamed event
 * or a state moved on one side would not fail to compile on the other: the
 * timeline would silently stop recognising rows, or draw a state the backend
 * does not mean. This holds the two to one list and one mapping.
 */
const SOURCE = readFileSync(
  fileURLToPath(new URL("../../../src-tauri/src/ledger/lifecycle.rs", import.meta.url)),
  "utf8",
);

function rustEvents(): string[] {
  const block = SOURCE.match(/pub const EVENTS: \[&str; (\d+)\] = \[([^\]]*)\];/s);
  if (!block) throw new Error("ledger/lifecycle.rs no longer declares `pub const EVENTS`");
  const names = [...block[2].matchAll(/"([a-z_]+)"/g)].map((m) => m[1]);
  expect(names).toHaveLength(Number(block[1]));
  return names;
}

function rustStates(): Map<string, string> {
  const body = SOURCE.match(/pub fn state_of\(event: &str\) -> Option<&'static str> \{(.*?)\n\}/s);
  if (!body) throw new Error("ledger/lifecycle.rs no longer declares `state_of`");
  const states = new Map<string, string>();
  for (const arm of body[1].matchAll(/((?:"[a-z_]+"\s*\|?\s*)+)=>\s*"([a-z_]+)"/g)) {
    for (const name of arm[1].matchAll(/"([a-z_]+)"/g)) states.set(name[1], arm[2]);
  }
  return states;
}

describe("the lifecycle vocabulary", () => {
  it("lists the events the backend records, no more and no fewer", () => {
    expect([...SESSION_STATE_EVENTS].sort()).toEqual(rustEvents().sort());
  });

  it("maps every event to the state the backend says it leaves a session in", () => {
    const states = rustStates();
    expect([...states.keys()].sort()).toEqual([...SESSION_STATE_EVENTS].sort());
    for (const event of SESSION_STATE_EVENTS) {
      expect(stateFor(event), event).toBe(states.get(event));
    }
  });
});
