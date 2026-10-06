import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { ACTIVITY_PUBLISH_MS, ATTENTION_EVENTS, MAX_TRACKED_ACTIVITY, createSessionActivity, isReaderInput, parseAttention } from "./sessionActivity";

const here = dirname(fileURLToPath(import.meta.url));

describe("the attention vocabulary matches the native notifier's", () => {
  it("names every hook event the bridge announces, and nothing it does not", () => {
    // Read from the Rust table rather than listed here, so an event added
    // there fails this test instead of reaching the pane as "Signalled".
    const bridge = readFileSync(join(here, "../../../src-tauri/src/alerts/bridge.rs"), "utf8");
    const table = bridge.slice(bridge.indexOf("pub const EVENTS"), bridge.indexOf("];", bridge.indexOf("pub const EVENTS")));
    const names = [...table.matchAll(/\(\s*"([a-z_]+)"\s*,/g)].map((match) => match[1]);
    expect(names.length).toBeGreaterThanOrEqual(6);
    expect(Object.keys(ATTENTION_EVENTS).sort()).toEqual([...names].sort());
  });

  it("listens on the event name the notifier emits", () => {
    const alerts = readFileSync(join(here, "../../../src-tauri/src/alerts/mod.rs"), "utf8");
    expect(alerts).toContain('pub const ATTENTION_EVENT: &str = "gitpulse-session-attention";');
  });

  it("parses the payload exactly as the notifier serializes it", () => {
    // The Rust test `attention_crosses_to_the_renderer_in_the_shape_it_parses`
    // pins these literals to `serde_json::to_string(&Attention)`; reading them
    // from there means neither side can change the shape alone.
    const tests = readFileSync(join(here, "../../../src-tauri/src/alerts/tests.rs"), "utf8");
    const wire = (name: string) => {
      const found = tests.match(new RegExp(`const ${name}: &str = r#"(.*)"#;`));
      if (!found) throw new Error(`${name} is missing from alerts/tests.rs`);
      return JSON.parse(found[1]) as unknown;
    };
    expect(parseAttention(wire("WIRE_HOOK"), 1_000)).toEqual({
      session: "term-1",
      attention: { kind: "needs-you", label: "Needs your permission", detail: "Bash: cargo test", at: 1_000 },
    });
    expect(parseAttention(wire("WIRE_SIGNAL"), 1_000)).toEqual({
      session: "term-1",
      attention: { kind: "signalled", label: "Signalled", detail: null, at: 1_000 },
    });
  });
});

describe("isReaderInput", () => {
  it("counts keys, text, Enter and Ctrl-C as the reader's", () => {
    for (const data of ["y", "\r", "\x03", "\x1b[A", "\x1bOB", "hello world", "\x1b[200~pasted\x1b[201~", "\x1b"]) {
      expect(isReaderInput(data), JSON.stringify(data)).toBe(true);
    }
  });

  it("does not count replies xterm sends on the program's behalf", () => {
    for (const data of ["\x1b[I", "\x1b[O", "\x1b[12;40R", "\x1b[?1;2c", "\x1b[>0;276;0c", "\x1b[?2004;1$y",
      "\x1b]11;rgb:0000/0000/0000\x07", "\x1b]10;rgb:ffff/ffff/ffff\x1b\\", "\x1b[<0;10;5M", "\x1b[M !!", "\x1b[I\x1b[12;40R", ""]) {
      expect(isReaderInput(data), JSON.stringify(data)).toBe(false);
    }
  });

  it("counts a keystroke that arrives in the same chunk as a reply", () => {
    expect(isReaderInput("\x1b[Iy")).toBe(true);
  });
});

describe("parseAttention", () => {
  it("turns a hook event into what it asks of the reader", () => {
    expect(parseAttention({ session: "term-1-a", channel: "hook", event: "permission_prompt", detail: "Bash: cargo test" }, 5))
      .toEqual({ session: "term-1-a", attention: { kind: "needs-you", label: "Needs your permission", detail: "Bash: cargo test", at: 5 } });
    expect(parseAttention({ session: "t", channel: "hook", event: "agent_completed" }, 5)?.attention.kind).toBe("finished");
    expect(parseAttention({ session: "t", channel: "hook", event: "error" }, 5)?.attention.kind).toBe("error");
  });

  it("calls a terminal signal, or an event it does not know, signalled — never needs-you", () => {
    expect(parseAttention({ session: "t", channel: "bell" }, 5)?.attention).toEqual({ kind: "signalled", label: "Signalled", detail: null, at: 5 });
    expect(parseAttention({ session: "t", channel: "hook", event: "constructor" }, 5)?.attention.kind).toBe("signalled");
    expect(parseAttention({ session: "t", channel: "hook", event: "future_event" }, 5)?.attention.kind).toBe("signalled");
  });

  it("refuses a malformed announcement rather than guessing", () => {
    for (const raw of [null, "x", [], {}, { session: "", channel: "hook" }, { session: "../x", channel: "hook" }, { session: "t" }, { session: "a".repeat(129), channel: "bell" }]) {
      expect(parseAttention(raw, 1), JSON.stringify(raw)).toBeNull();
    }
  });

  it("strips control characters and bounds what the program said", () => {
    const parsed = parseAttention({ session: "t", channel: "osc9", detail: `\x1b[31m${"z".repeat(5000)}\x07` }, 1);
    expect(parsed?.attention.detail?.length).toBeLessThanOrEqual(240);
    expect(parsed?.attention.detail).not.toMatch(/[\x00-\x1f]/);
    expect(parseAttention({ session: "t", channel: "osc9", detail: "   " }, 1)?.attention.detail).toBeNull();
  });
});

describe("createSessionActivity", () => {
  let now = 1_000_000;
  beforeEach(() => { vi.useFakeTimers(); now = 1_000_000; });
  afterEach(() => { vi.useRealTimers(); });
  const make = () => createSessionActivity(() => now);

  it("keeps attention until the reader types, however much the agent prints", () => {
    const activity = make();
    activity.announce({ session: "term-1", channel: "hook", event: "agent_completed" });
    for (let i = 0; i < 20; i += 1) { now += 300; activity.output("term-1"); }
    activity.input("term-1", "\x1b[I");
    expect(get(activity).get("term-1")?.attention?.kind).toBe("finished");
    activity.input("term-1", "y");
    expect(get(activity).get("term-1")?.attention).toBeNull();
  });

  it("replaces an older announcement with the newer one", () => {
    const activity = make();
    activity.announce({ session: "term-1", channel: "hook", event: "agent_completed" });
    now += 10;
    activity.announce({ session: "term-1", channel: "hook", event: "permission_prompt" });
    expect(get(activity).get("term-1")?.attention).toMatchObject({ kind: "needs-you", at: now });
  });

  it("publishes output at most once a second per session, and the last one still lands", () => {
    const activity = make();
    let publishes = 0;
    const stop = activity.subscribe(() => { publishes += 1; });
    publishes = 0;
    for (let i = 0; i < 100; i += 1) { now += 10; activity.output("term-1"); }
    expect(publishes).toBe(1);
    now += ACTIVITY_PUBLISH_MS;
    vi.advanceTimersByTime(ACTIVITY_PUBLISH_MS);
    expect(publishes).toBe(2);
    expect(get(activity).get("term-1")?.lastOutputAt).toBe(1_000_000 + 1000);
    stop();
  });

  it("treats a title change as a sign of life and keeps the title clean", () => {
    const activity = make();
    activity.title("term-1", "\x1b]0;✳ Fixing the importer\x07");
    expect(get(activity).get("term-1")).toMatchObject({ title: "]0;✳ Fixing the importer", lastOutputAt: now });
    activity.title("term-2", "x".repeat(1000));
    vi.advanceTimersByTime(ACTIVITY_PUBLISH_MS);
    expect(get(activity).get("term-2")?.title?.length).toBe(120);
  });

  it("forgets a session that ended, and ignores ids that are not session ids", () => {
    const activity = make();
    activity.output("term-1");
    activity.forget("term-1");
    expect(get(activity).has("term-1")).toBe(false);
    activity.output("../../etc");
    activity.output("");
    vi.advanceTimersByTime(ACTIVITY_PUBLISH_MS);
    expect(get(activity).size).toBe(0);
  });

  it("stays bounded, giving way the longest-quiet session first", () => {
    const activity = make();
    for (let i = 0; i < MAX_TRACKED_ACTIVITY + 25; i += 1) { now += 1; activity.output(`term-${i}`); }
    vi.advanceTimersByTime(ACTIVITY_PUBLISH_MS);
    const tracked = get(activity);
    expect(tracked.size).toBe(MAX_TRACKED_ACTIVITY);
    expect(tracked.has("term-0")).toBe(false);
    expect(tracked.has(`term-${MAX_TRACKED_ACTIVITY + 24}`)).toBe(true);
  });
});
