/**
 * Stress: the agent-defaults rules under volume, junk, and every combination.
 *
 * The unit tests pick representative cases. This one sweeps the whole space —
 * every launcher against every mode, deep and hostile payloads, and the one
 * invariant that must survive all of it:
 *
 *   **a mode that cannot be applied must never read as one that was, and
 *   bypass must never be reachable without an acknowledgement.**
 *
 * Bounded on purpose. These are pure functions, so the useful axis is input
 * shape rather than wall-clock; a timing assertion here would measure the CI
 * runner's load and nothing about this code.
 */
import { describe, expect, it } from "vitest";
import { LAUNCHERS, type LauncherKind } from "./tabs";
import {
  BYPASS_MODE,
  PERMISSION_LAUNCHERS,
  PERMISSION_MODES,
  effectiveMode,
  isPermissionMode,
  requiresAcknowledgement,
  sanitizeAgentDefaults,
  type AgentDefaults,
} from "./agentDefaults";

const ALL_LAUNCHERS = LAUNCHERS.map((entry) => entry.kind);

describe("the full launcher x mode grid", () => {
  it("resolves every combination without throwing, and only ever to a real mode", () => {
    let resolved = 0;
    let ignored = 0;
    for (const launcher of ALL_LAUNCHERS) {
      for (const mode of PERMISSION_MODES) {
        const stored = { permission: { [launcher]: mode } } as unknown as AgentDefaults;
        const effective = effectiveMode(stored, launcher);
        if (effective === null) {
          ignored += 1;
          // Only launchers with no policy may resolve to null here.
          expect(PERMISSION_LAUNCHERS).not.toContain(launcher);
          continue;
        }
        resolved += 1;
        expect(isPermissionMode(effective)).toBe(true);
        expect(effective).toBe(mode);
        expect(PERMISSION_LAUNCHERS).toContain(launcher);
      }
    }
    // Both halves are non-empty, so neither branch is silently untested. Carrying
    // both numbers rather than one total: "6 resolved" and "6 resolved, 6
    // ignored" are different facts about this grid.
    expect(resolved).toBe(PERMISSION_LAUNCHERS.length * PERMISSION_MODES.length);
    expect(ignored).toBe(
      (ALL_LAUNCHERS.length - PERMISSION_LAUNCHERS.length) * PERMISSION_MODES.length,
    );
    expect(ignored).toBeGreaterThan(0);
  });

  it("asks for an acknowledgement in exactly the bypass cells and nowhere else", () => {
    let gated = 0;
    for (const launcher of PERMISSION_LAUNCHERS) {
      for (const mode of PERMISSION_MODES) {
        const stored = { permission: { [launcher]: mode } } as AgentDefaults;
        const needs = requiresAcknowledgement(effectiveMode(stored, launcher));
        expect(needs).toBe(mode === BYPASS_MODE);
        if (needs) gated += 1;
      }
    }
    expect(gated).toBe(PERMISSION_LAUNCHERS.length);
  });

  /**
   * The failure this whole design exists to prevent: a stored bypass reaching
   * a launch without the reader agreeing to it. Swept over every launcher and
   * every way the surrounding state could be wrong.
   */
  it("never yields an un-acknowledged bypass, whatever else is malformed", () => {
    const hostile: unknown[] = [
      { permission: { claude: BYPASS_MODE } },
      { permission: { claude: BYPASS_MODE, nope: BYPASS_MODE } },
      JSON.parse(`{"permission":{"claude":"${BYPASS_MODE}","__proto__":"${BYPASS_MODE}"}}`),
      { permission: { claude: BYPASS_MODE }, extra: { permission: { claude: "ask" } } },
    ];
    for (const raw of hostile) {
      const sanitized = sanitizeAgentDefaults(raw);
      for (const launcher of ALL_LAUNCHERS) {
        const mode = effectiveMode(sanitized, launcher);
        if (mode === BYPASS_MODE) {
          // Reachable only as a mode that demands an acknowledgement.
          expect(requiresAcknowledgement(mode)).toBe(true);
        }
      }
    }
  });
});

describe("sanitize under volume and junk", () => {
  it("drops a large map of unknown launchers without keeping any of it", () => {
    const permission: Record<string, string> = {};
    for (let i = 0; i < 5_000; i += 1) permission[`launcher-${i}`] = BYPASS_MODE;
    const result = sanitizeAgentDefaults({ permission });
    expect(result).toEqual({ permission: {} });
  });

  it("keeps the valid entries out of a large mixed map", () => {
    const permission: Record<string, string> = {};
    for (let i = 0; i < 5_000; i += 1) permission[`launcher-${i}`] = "edit";
    for (const launcher of PERMISSION_LAUNCHERS) permission[launcher] = "inspect";
    const result = sanitizeAgentDefaults({ permission });
    expect(Object.keys(result.permission).sort()).toEqual([...PERMISSION_LAUNCHERS].sort());
    for (const launcher of PERMISSION_LAUNCHERS) {
      expect(result.permission[launcher]).toBe("inspect");
    }
  });

  it("survives deeply nested and self-referential input", () => {
    // A 10k-deep object reaches sanitize as a value it simply does not
    // recognise; what must not happen is a throw on the spawn path.
    let deep: Record<string, unknown> = { permission: { claude: "edit" } };
    for (let i = 0; i < 10_000; i += 1) deep = { nested: deep };
    expect(() => sanitizeAgentDefaults(deep)).not.toThrow();
    expect(sanitizeAgentDefaults(deep)).toEqual({ permission: {} });

    const cyclic: Record<string, unknown> = { permission: { claude: "edit" } };
    cyclic.self = cyclic;
    expect(sanitizeAgentDefaults(cyclic)).toEqual({ permission: { claude: "edit" } });
  });

  it("is not confused by launcher names that only look valid", () => {
    const lookalikes = [
      "Claude",
      "claude ",
      " claude",
      "claude\n",
      "claude\u0000",
      "cláude",
      "claude-code",
      "CLAUDE",
      "claude\u200b",
    ];
    for (const name of lookalikes) {
      const result = sanitizeAgentDefaults({ permission: { [name]: BYPASS_MODE } });
      expect(result.permission, `${JSON.stringify(name)} was accepted as a launcher`).toEqual({});
    }
  });

  it("is not confused by mode names that only look valid", () => {
    const lookalikes = ["Bypass", "bypass ", " bypass", "bypass\u0000", "by\u200bpass", "BYPASS"];
    for (const mode of lookalikes) {
      const result = sanitizeAgentDefaults({ permission: { claude: mode } });
      expect(result.permission, `${JSON.stringify(mode)} was accepted as a mode`).toEqual({});
    }
  });

  it("is idempotent across the whole grid, so saving what was read changes nothing", () => {
    for (const launcher of ALL_LAUNCHERS) {
      for (const mode of PERMISSION_MODES) {
        const once = sanitizeAgentDefaults({ permission: { [launcher]: mode } });
        const twice = sanitizeAgentDefaults(once);
        const thrice = sanitizeAgentDefaults(twice);
        expect(twice).toEqual(once);
        expect(thrice).toEqual(once);
      }
    }
  });

  it("narrows to whatever launcher list it is handed, for every subset", () => {
    const full = { permission: Object.fromEntries(PERMISSION_LAUNCHERS.map((l) => [l, "edit"])) };
    // Every subset of the supported launchers, so a downlevel backend naming
    // any combination is covered rather than just the empty and full cases.
    const n = PERMISSION_LAUNCHERS.length;
    for (let mask = 0; mask < 1 << n; mask += 1) {
      const subset = PERMISSION_LAUNCHERS.filter((_, i) => mask & (1 << i));
      const result = sanitizeAgentDefaults(full, subset as LauncherKind[]);
      expect(Object.keys(result.permission).sort()).toEqual([...subset].sort());
    }
  });
});
