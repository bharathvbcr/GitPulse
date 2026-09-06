/**
 * Contract: the tab ceiling is the backend's session ceiling.
 *
 * Derived from the Rust source rather than written down twice. A UI that
 * stopped short of `MAX_PTY_SESSIONS` would make a reachable backend refusal
 * unreachable; a UI that ran past it would surface that refusal as an
 * unexplained "Failed to spawn" on a tab the user was invited to open. Either
 * way the number that matters is the one in Rust, so this reads it.
 */
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { MAX_TERMINAL_TABS } from "./tabs";

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
const rust = readFileSync(join(repoRoot, "src-tauri", "src", "terminal", "mod.rs"), "utf8");

describe("terminal tab ceiling", () => {
  it("matches MAX_PTY_SESSIONS in the Rust terminal module", () => {
    const match = rust.match(/const MAX_PTY_SESSIONS:\s*usize\s*=\s*(\d+)\s*;/);
    // A rename or a reshaped declaration must fail loudly here rather than
    // silently stop checking anything — an unfindable constant is not a
    // matching one.
    expect(match, "MAX_PTY_SESSIONS declaration not found in src-tauri/src/terminal/mod.rs").not
      .toBeNull();
    expect(MAX_TERMINAL_TABS).toBe(Number(match?.[1]));
  });

  it("is the ceiling the reservation actually enforces", () => {
    // The constant existing is not the same as it gating anything.
    expect(rust).toContain("(current < MAX_PTY_SESSIONS).then_some(current + 1)");
  });
});
