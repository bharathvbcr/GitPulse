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
import { LAUNCHERS, MAX_TERMINAL_TABS } from "./tabs";
import { DEFAULT_SESSION_ALERT_SETTINGS } from "../stores/sessionAlertsStore";

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
const rust = readFileSync(join(repoRoot, "src-tauri", "src", "terminal", "mod.rs"), "utf8");
const toolConfig = readFileSync(join(repoRoot, "src-tauri", "src", "tool_config.rs"), "utf8");

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

/**
 * Contract: every agent launcher the tab strip offers is one the backend
 * recognises by name.
 *
 * The backend uses that recognition for three things a user can see — whether
 * a session's bell is worth a banner, what the banner calls the agent, and
 * whether the ledger records the session as an agent's or a human's. A
 * launcher the strip offers but `AGENT_LAUNCHERS` omits is a tab that can
 * never notify, silently.
 */
describe("agent launcher names", () => {
  const declared = (() => {
    const match = rust.match(/pub const AGENT_LAUNCHERS:\s*\[&str;\s*\d+\]\s*=\s*\[([^\]]*)\]/);
    expect(match, "AGENT_LAUNCHERS declaration not found in src-tauri/src/terminal/mod.rs").not
      .toBeNull();
    return [...(match?.[1] ?? "").matchAll(/"([^"]+)"/g)].map((entry) => entry[1]);
  })();

  it("covers every launcher except the user's own shell", () => {
    const strip = LAUNCHERS.map((launcher) => launcher.kind).filter((kind) => kind !== "shell");
    expect([...declared].sort()).toEqual([...strip].sort());
  });

  it("does not claim the plain shell is an agent", () => {
    expect(declared).not.toContain("shell");
  });
});

/**
 * Contract: the settings the panel renders before the backend answers are the
 * settings the backend would have given it.
 *
 * The first paint of the notification panel uses these, and a terminal session
 * reads `configure_agents` from them to build its own argv. A default that
 * disagreed with Rust's would show the wrong switches and, worse, launch a CLI
 * with flags the user had turned off.
 */
describe("session notification defaults", () => {
  const block = toolConfig.match(
    /impl Default for SessionAlertSettings \{\s*fn default\(\) -> Self \{\s*Self \{([\s\S]*?)\n\s*\}\s*\n\s*\}\s*\n\}/,
  );

  it("was found in the Rust source", () => {
    expect(block, "SessionAlertSettings::default not found in src-tauri/src/tool_config.rs").not
      .toBeNull();
  });

  it("matches field for field", () => {
    const body = block?.[1] ?? "";
    const rustDefaults: Record<string, unknown> = {};
    for (const [, key, value] of body.matchAll(/(\w+):\s*([^,\n]+),/g)) {
      rustDefaults[key] = value.trim() === "true" ? true : value.trim() === "false" ? false
        : value.trim() === "None" ? null : value.trim();
    }
    expect(rustDefaults).toEqual({ ...DEFAULT_SESSION_ALERT_SETTINGS });
  });
});
