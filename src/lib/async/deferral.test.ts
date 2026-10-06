/**
 * Contract: the deferral marker is the backend's, read from the Rust source
 * rather than written down twice. A drifted marker would turn every deferral
 * back into an ERROR toast and an immediate retry.
 */
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import {
  DEFERRED_UNDER_LOAD_MARKER,
  MAX_DEFERRED_RETRIES,
  deferredRetryDelayMs,
  isDeferredUnderLoad,
  outcomeUnknown,
  RUN_TIMEOUT_MARKER,
  SLOT_WAIT_SUFFIX,
} from "./deferral";

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
const gitCli = readFileSync(join(repoRoot, "src-tauri", "src", "engine", "git_cli.rs"), "utf8");

describe("deferred-under-load marker", () => {
  it("matches DEFERRED_MARKER in the Rust spawn gate", () => {
    const match = gitCli.match(/const DEFERRED_MARKER:\s*&str\s*=\s*"([^"]*)"\s*;/);
    expect(match, "DEFERRED_MARKER declaration not found in src-tauri/src/engine/git_cli.rs").not
      .toBeNull();
    expect(DEFERRED_UNDER_LOAD_MARKER).toBe(match?.[1]);
  });

  it("reads the timeout marker and slot-wait suffix from the Rust source", () => {
    const marker = gitCli.match(/const TIMEOUT_MARKER:\s*&str\s*=\s*"([^"]*)"\s*;/);
    const suffix = gitCli.match(/const SLOT_WAIT_SUFFIX:\s*&str\s*=\s*"([^"]*)"\s*;/);
    expect(marker, "TIMEOUT_MARKER declaration not found in git_cli.rs").not.toBeNull();
    expect(suffix, "SLOT_WAIT_SUFFIX declaration not found in git_cli.rs").not.toBeNull();
    expect(RUN_TIMEOUT_MARKER).toBe(marker?.[1]);
    expect(SLOT_WAIT_SUFFIX).toBe(suffix?.[1]);
  });

  it("calls a write's outcome unknown only when it started and hit its deadline", () => {
    expect(outcomeUnknown("gh timed out after 90s")).toBe(true);
    expect(outcomeUnknown("gh timed out after 90.000s waiting for a process slot")).toBe(false);
    expect(outcomeUnknown("gh deferred under load after 2.000s: the git spawn rate limit admitted nothing sooner")).toBe(false);
    expect(outcomeUnknown("gh cancelled before spawn")).toBe(false);
    expect(outcomeUnknown("gh: To get started with GitHub CLI, please run: gh auth login")).toBe(false);
    expect(outcomeUnknown("")).toBe(false);
  });

  it("recognises a gate deferral and nothing that merely times out", () => {
    expect(
      isDeferredUnderLoad(
        "git status deferred under load after 2.013s: the git spawn rate limit admitted nothing sooner",
      ),
    ).toBe(true);
    expect(isDeferredUnderLoad("git status timed out after 90.000s waiting for a process slot")).toBe(
      false,
    );
    expect(isDeferredUnderLoad("git status timed out after 90s")).toBe(false);
  });

  it("never retries inside the gate's 2 s queue budget, and stays bounded", () => {
    const delays = Array.from({ length: MAX_DEFERRED_RETRIES + 3 }, (_, i) =>
      deferredRetryDelayMs(i + 1),
    );
    expect(delays[0]).toBeGreaterThan(2_000);
    for (let i = 1; i < delays.length; i += 1) {
      expect(delays[i]).toBeGreaterThanOrEqual(delays[i - 1]);
    }
    expect(Math.max(...delays)).toBe(30_000);
    expect(deferredRetryDelayMs(0)).toBe(deferredRetryDelayMs(1));
  });
});
