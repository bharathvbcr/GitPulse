import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

import { attentionWrite, type Attention } from "../src/lib/workbench/client";

/**
 * Snooze exists twice: the activity inbox's button, which writes
 * `attention.update` from the renderer, and the macOS banner's button, which
 * the native coordinator turns into the same write. One inbox row is behind
 * both, so they must defer it by the same amount — a banner promising an hour
 * while the row comes back in a different time would make one of them lie.
 *
 * The renderer's number is read from what `attentionWrite` actually sends,
 * not from its source text; the Rust number is the constant the coordinator
 * sends and the banner's title is derived from.
 */
const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

function rustSnoozeSeconds(): number {
  const source = readFileSync(path.join(ROOT, "src-tauri/src/workbench/notifications.rs"), "utf8");
  const match = source.match(/pub\(super\) const SNOOZE_SECONDS: u32 = (\d+);/);
  if (!match) throw new Error("SNOOZE_SECONDS is no longer a u32 literal this test can read");
  return Number(match[1]);
}

describe("notification snooze duration", () => {
  it("is the same for the inbox row and the native banner", () => {
    const notice = { id: "event-1", revision: 1 } as Attention;
    const write = attentionWrite(notice, "snooze");
    expect(write.seconds).toBe(rustSnoozeSeconds());
  });

  it("is a whole number of hours, which the banner's title assumes", () => {
    // macos.rs titles the action `Snooze {SNOOZE_SECONDS / 3600} hour`.
    expect(rustSnoozeSeconds() % 3600).toBe(0);
    expect(rustSnoozeSeconds()).toBeGreaterThan(0);
  });
});
