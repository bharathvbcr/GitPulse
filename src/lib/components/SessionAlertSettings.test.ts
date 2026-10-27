/**
 * What the settings panel says about hook reports that got no reply
 * (ft-6d8ff969, item 4).
 *
 * `environment: "node"` renders a component on the server, where the panel's
 * `load` effect never runs and `draft` stays null, so the counts block cannot
 * be reached by `render` here. The sentence therefore lives in one pure
 * function of the status (`hookReplyMisses`), tested on its output, and the
 * template is read for the three things the function cannot say for itself:
 * where the line sits, that the error is its own alert, and that nothing sums
 * the count with another.
 */
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import SessionAlertSettings from "./SessionAlertSettings.svelte";
import { hookReplyMisses, type SessionAlertStatus } from "../stores/sessionAlertsStore";

const source = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "SessionAlertSettings.svelte"), "utf8");

function status(overrides: Partial<SessionAlertStatus> = {}): SessionAlertStatus {
  return {
    running: true,
    delivered: 0,
    suppressed_disabled: 0,
    suppressed_not_agent: 0,
    suppressed_attended: 0,
    suppressed_quiet: 0,
    coalesced: 0,
    resolved: 0,
    displaced: 0,
    rate_limited: 0,
    deferred: 0,
    dropped_queue: 0,
    dropped_scan: 0,
    failed: 0,
    bridge_rejected: 0,
    stale: 0,
    hook_replies_missed: 0,
    hook_reply_missed_last: null,
    hook_drop_record_error: null,
    bridge_listening: true,
    bridge_path: null,
    last_error: null,
    ...overrides,
  };
}

describe("the line about hook reports that got no reply", () => {
  it("says the count, the window, the newest event and time, and what a miss can leave behind", () => {
    const { line } = hookReplyMisses(
      status({ hook_replies_missed: 3, hook_reply_missed_last: "session_ended at 2026-10-27T12:00:00.000Z" }),
    );
    expect(line).toBe(
      "3 hook reports in the last 7 days got no reply from GitPulse in time " +
        "(last: session_ended at 2026-10-27T12:00:00.000Z). " +
        "A missed SessionEnd can leave that agent's banner standing; hooks do not retry.",
    );
  });

  it("is singular for one, and leaves out a last event it was not given", () => {
    const { line } = hookReplyMisses(status({ hook_replies_missed: 1 }));
    expect(line).toMatch(/^1 hook report in the last 7 days got no reply from GitPulse in time\. A missed/);
    expect(line).not.toContain("last:");
  });

  it("is the hook's own count: independent of bridge_rejected, and never called lost", () => {
    const base = hookReplyMisses(status({ hook_replies_missed: 2 }));
    const withRejections = hookReplyMisses(status({ hook_replies_missed: 2, bridge_rejected: 40 }));
    expect(withRejections).toEqual(base);
    expect(base.line).toContain("2 hook reports");
    expect(base.line).not.toContain("42");
    for (const text of [base.line, hookReplyMisses(status({ hook_drop_record_error: "x" })).error]) {
      expect(text ?? "").not.toMatch(/lost/i);
    }
  });

  it("says nothing when none were missed and the record read cleanly", () => {
    expect(hookReplyMisses(status())).toEqual({ line: null, error: null });
  });

  it("gives an unreadable record as its own error, with no clean zero beside it", () => {
    const misses = hookReplyMisses(status({ hook_drop_record_error: "could not be read in full" }));
    expect(misses.line).toBeNull();
    expect(misses.error).toBe("could not be read in full");
  });

  it("keeps a floor and the error together when part of the record was read", () => {
    const misses = hookReplyMisses(status({ hook_replies_missed: 2, hook_drop_record_error: "prev unreadable" }));
    expect(misses.line).toContain("2 hook reports");
    expect(misses.error).toBe("prev unreadable");
  });
});

describe("the panel's template", () => {
  const counts = source.slice(source.indexOf('data-testid="session-alerts-counts"'));
  const block = counts.slice(counts.indexOf("{#if view.bridge_supported}"));

  it("shows the line only where the hook bridge is supported, from the shared function", () => {
    expect(block).toContain("misses.line");
    expect(block.indexOf("session-alerts-hook-misses")).toBeGreaterThan(-1);
    expect(source).toContain("hookReplyMisses(view.status)");
  });

  it("renders the record's error as its own role=alert paragraph, not inside the line", () => {
    expect(source).toMatch(
      /<p role="alert" data-testid="session-alerts-hook-record-error">\{misses\.error\}<\/p>/,
    );
    expect(source).toMatch(/<p data-testid="session-alerts-hook-misses">\{misses\.line\}<\/p>/);
  });

  it("never sums the count with bridge_rejected or any other counter", () => {
    const own = source.slice(
      source.indexOf("const misses"),
      source.indexOf("async function load"),
    );
    expect(own).not.toContain("bridge_rejected");
    expect(own).not.toContain("lost");
    expect(source).not.toMatch(/hook_replies_missed\s*\+|\+\s*view\.status\.hook_replies_missed/);
    expect(source).not.toMatch(/bridge_rejected\s*\+|\+\s*view\.status\.bridge_rejected/);
    // The lost-signals total keeps its own three counters and no fourth.
    expect(source).toContain("view.status.dropped_queue + view.status.dropped_scan + view.status.displaced");
  });

  it("still renders on the server before its settings have loaded, without the line", () => {
    const html = render(SessionAlertSettings).body;
    expect(html).toContain('data-testid="session-alerts"');
    expect(html).not.toContain("session-alerts-hook-misses");
  });
});
