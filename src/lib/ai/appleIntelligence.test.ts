import { describe, expect, it } from "vitest";
import type { HostOS } from "../platform";
import {
  parseAppleStatus,
  showsAppleOption,
  unknownAppleStatus,
  type AppleIntelligenceStatus,
} from "./appleIntelligence";

const ok: AppleIntelligenceStatus = {
  available: true,
  state: { state: "available" },
  explanation: "On-device drafting is ready.",
};

describe("the pre-probe answer", () => {
  /**
   * Denying is the only safe default. Offering on-device drafting because the
   * probe has not answered yet would put the reader through a generation that
   * cannot run.
   */
  it("denies availability on every host", () => {
    for (const os of ["macos", "windows", "linux", "unknown"] as HostOS[]) {
      expect(unknownAppleStatus(os).available).toBe(false);
    }
  });

  it("does not blame a Mac for a probe that has not finished", () => {
    const status = unknownAppleStatus("macos");
    expect(status.state.state).toBe("unavailable");
    expect(status.explanation).toMatch(/Checking/);
    expect(status.explanation).not.toMatch(/does not support/);
  });

  it("names the platform on a host that can never support it", () => {
    const status = unknownAppleStatus("windows");
    expect(status.state).toEqual({ state: "unsupported_os", os: "windows" });
    expect(status.explanation).toMatch(/windows/);
  });
});

describe("parsing the status envelope", () => {
  it("accepts a well-formed available reply", () => {
    const parsed = parseAppleStatus(
      { available: true, state: { state: "available" }, explanation: "Ready." },
      "macos",
    );
    expect(parsed.available).toBe(true);
    expect(parsed.explanation).toBe("Ready.");
  });

  it("keeps each unavailable cause distinguishable", () => {
    const notCompiled = parseAppleStatus(
      { available: false, state: { state: "not_compiled" }, explanation: "No bridge." },
      "macos",
    );
    expect(notCompiled.state.state).toBe("not_compiled");

    const refused = parseAppleStatus(
      {
        available: false,
        state: { state: "unavailable", reason: "apple_intelligence_not_enabled" },
        explanation: "Turn it on.",
      },
      "macos",
    );
    expect(refused.state).toEqual({
      state: "unavailable",
      reason: "apple_intelligence_not_enabled",
    });
    expect(notCompiled.state.state).not.toBe(refused.state.state);
  });

  /**
   * `available: true` with a state that is not `available` is a backend fault.
   * The safe reading of a fault is "not available", never the optimistic one.
   */
  it("refuses a reply whose flag contradicts its state", () => {
    const parsed = parseAppleStatus(
      { available: true, state: { state: "not_compiled" }, explanation: "Contradiction." },
      "macos",
    );
    expect(parsed.available).toBe(false);
  });

  it.each([
    ["null", null],
    ["a string", "available"],
    ["an empty object", {}],
    ["a missing state", { available: true, explanation: "x" }],
    ["an unknown state tag", { available: true, state: { state: "maybe" }, explanation: "x" }],
    ["a truthy non-boolean flag", { available: "yes", state: { state: "available" }, explanation: "x" }],
  ])("denies availability given %s", (_label, value) => {
    expect(parseAppleStatus(value, "macos").available).toBe(false);
  });

  it("falls back to a real sentence when the explanation is blank", () => {
    const parsed = parseAppleStatus(
      { available: false, state: { state: "not_compiled" }, explanation: "   " },
      "macos",
    );
    expect(parsed.explanation.trim().length).toBeGreaterThan(0);
  });
});

describe("whether the option is offered", () => {
  it("offers it on a Mac that is ready", () => {
    expect(showsAppleOption("macos", ok)).toBe(true);
  });

  /**
   * On macOS the reason is usually actionable — Apple Intelligence switched off,
   * or a model still downloading — so the control stays visible to carry it.
   */
  it("still offers it on a Mac where the reason is actionable", () => {
    for (const reason of ["apple_intelligence_not_enabled", "model_not_ready"]) {
      expect(
        showsAppleOption("macos", {
          available: false,
          state: { state: "unavailable", reason },
          explanation: "…",
        }),
      ).toBe(true);
    }
  });

  it("hides it on every non-macOS host", () => {
    for (const os of ["windows", "linux", "unknown"] as HostOS[]) {
      expect(showsAppleOption(os, ok)).toBe(false);
      expect(
        showsAppleOption(os, {
          available: false,
          state: { state: "unsupported_os", os },
          explanation: "…",
        }),
      ).toBe(false);
    }
  });

  /** A build with no bridge cannot be fixed from a per-task control. */
  it("hides it when this build has no bridge", () => {
    expect(
      showsAppleOption("macos", {
        available: false,
        state: { state: "not_compiled" },
        explanation: "…",
      }),
    ).toBe(false);
  });

  /** A backend that reports unsupported_os outranks the webview's guess. */
  it("hides it when the backend says the OS is unsupported, even if the webview says macOS", () => {
    expect(
      showsAppleOption("macos", {
        available: false,
        state: { state: "unsupported_os", os: "linux" },
        explanation: "…",
      }),
    ).toBe(false);
  });
});
