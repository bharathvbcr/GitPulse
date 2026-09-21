/**
 * The rules that decide what a stored preference is allowed to become.
 *
 * Everything here is about a file a user can hand-edit and a backend that may
 * be a different version from this app. The invariant under all of it: a mode
 * that cannot be applied must never read as one that was.
 */
import { describe, expect, it } from "vitest";
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

describe("isPermissionMode", () => {
  it("accepts every declared mode and nothing else", () => {
    for (const mode of PERMISSION_MODES) expect(isPermissionMode(mode)).toBe(true);
    for (const value of [
      "",
      " ",
      "plan",
      "yolo",
      "INSPECT",
      "bypass ",
      null,
      undefined,
      0,
      1,
      {},
      [],
      ["bypass"],
      { toString: () => "bypass" },
      Object.create({ bypass: true }),
    ]) {
      expect(isPermissionMode(value), `${String(value)} was accepted`).toBe(false);
    }
  });
});

describe("effectiveMode", () => {
  const defaults: AgentDefaults = { permission: { claude: "edit", codex: BYPASS_MODE } };

  it("returns the stored mode for a supported launcher", () => {
    expect(effectiveMode(defaults, "claude")).toBe("edit");
    expect(effectiveMode(defaults, "codex")).toBe(BYPASS_MODE);
  });

  it("returns null when nothing is stored, which means the CLI's own default", () => {
    expect(effectiveMode(defaults, "grok")).toBeNull();
    expect(effectiveMode({ permission: {} }, "claude")).toBeNull();
  });

  /**
   * A hand-edited file can name a launcher with no policy. Returning null
   * means the launch never asks for a mode, so the backend's refusal is never
   * reached — while still never pretending a mode was applied.
   */
  it("ignores a stored mode for a launcher the backend has no policy for", () => {
    const stored = { permission: { shell: "edit", manvi: BYPASS_MODE } } as unknown as AgentDefaults;
    expect(effectiveMode(stored, "shell")).toBeNull();
    expect(effectiveMode(stored, "manvi")).toBeNull();
  });

  it("honours a narrowed supported list, as a downlevel backend would send", () => {
    // The backend is the authority on which launchers have a policy. If it
    // says only claude does, a stored codex mode must not be applied.
    expect(effectiveMode(defaults, "codex", ["claude"])).toBeNull();
    expect(effectiveMode(defaults, "claude", ["claude"])).toBe("edit");
  });

  it("treats a junk stored value as absent rather than coercing it", () => {
    const junk = { permission: { claude: "yolo" } } as unknown as AgentDefaults;
    expect(effectiveMode(junk, "claude")).toBeNull();
  });
});

describe("requiresAcknowledgement", () => {
  it("is true only for bypass", () => {
    expect(requiresAcknowledgement(BYPASS_MODE)).toBe(true);
    expect(requiresAcknowledgement(null)).toBe(false);
    for (const mode of PERMISSION_MODES.filter((m) => m !== BYPASS_MODE)) {
      expect(requiresAcknowledgement(mode), `${mode} asked for an acknowledgement`).toBe(false);
    }
  });
});

describe("sanitizeAgentDefaults", () => {
  it("returns an empty default for anything that is not an object", () => {
    for (const value of [null, undefined, 0, "", "x", [], true, NaN]) {
      expect(sanitizeAgentDefaults(value)).toEqual({ permission: {} });
    }
  });

  it("keeps supported launchers with valid modes", () => {
    expect(
      sanitizeAgentDefaults({ permission: { claude: "edit", grok: "inspect" } }),
    ).toEqual({ permission: { claude: "edit", grok: "inspect" } });
  });

  it("drops unknown launchers and unknown modes, keeping the rest", () => {
    // One bad key must not discard the good ones: a preference file is edited
    // by hand, and losing every setting over one typo is its own defect.
    expect(
      sanitizeAgentDefaults({
        permission: { claude: "edit", shell: "edit", manvi: "ask", codex: "yolo", nope: "ask" },
      }),
    ).toEqual({ permission: { claude: "edit" } });
  });

  /**
   * Deliberately NOT stripped. Bypass is a legitimate stored preference; what
   * makes it safe is the acknowledgement asked at launch, not the parser
   * pretending it was never chosen. Stripping it here would silently
   * downgrade a choice the reader made and confirmed.
   */
  it("preserves a stored bypass rather than quietly downgrading it", () => {
    expect(sanitizeAgentDefaults({ permission: { claude: BYPASS_MODE } })).toEqual({
      permission: { claude: BYPASS_MODE },
    });
  });

  /**
   * Which launcher a new tab starts already has an owner —
   * `interfaceStore.terminalLauncher`. A second stored answer here would be
   * how two settings come to disagree, so an incoming one is discarded with
   * every other unknown key rather than quietly kept.
   */
  it("does not adopt a default launcher, which another store owns", () => {
    const result = sanitizeAgentDefaults({ permission: {}, launcher: "claude" });
    expect(result).toEqual({ permission: {} });
    expect("launcher" in result).toBe(false);
  });

  it("survives prototype-polluting and exotic shapes", () => {
    const hostile = JSON.parse('{"permission":{"__proto__":"edit","constructor":"edit"}}');
    expect(sanitizeAgentDefaults(hostile)).toEqual({ permission: {} });
    expect(({} as Record<string, unknown>).claude).toBeUndefined();
    expect(sanitizeAgentDefaults({ permission: "edit" })).toEqual({ permission: {} });
    expect(sanitizeAgentDefaults({ permission: null })).toEqual({ permission: {} });
    expect(sanitizeAgentDefaults({ permission: [] })).toEqual({ permission: {} });
  });

  it("round-trips its own output unchanged", () => {
    // The panel saves what it read; a sanitizer that changed a valid value on
    // the second pass would rewrite a reader's settings behind their back.
    for (const launcher of PERMISSION_LAUNCHERS) {
      for (const mode of PERMISSION_MODES) {
        const once = sanitizeAgentDefaults({ permission: { [launcher]: mode } });
        expect(sanitizeAgentDefaults(once)).toEqual(once);
      }
    }
  });
});
