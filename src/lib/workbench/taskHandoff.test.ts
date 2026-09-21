import { describe, expect, it } from "vitest";
import {
  MAX_CHECKOUT_LENGTH,
  checkoutBesideCommonDir,
  checkoutCandidates,
  defaultHandoff,
  describeHandoff,
  handoffGate,
  isAgentProvider,
  normalizeCheckout,
  preferredCheckout,
  reconcileHandoff,
  runStateLabel,
  sanitizeHandoff,
  supportsManaged,
} from "./taskHandoff";

const options = { caseInsensitive: false };
const repositories = [
  { id: "r0", identity_key: "local:/work/GitPulse/.git" },
  { id: "r1", identity_key: "local:/work/bare.git" },
  { id: "r2", identity_key: "weird:not-a-path" },
];

describe("sanitizeHandoff", () => {
  it("falls back to the default for anything unrecognizable", () => {
    expect(sanitizeHandoff(undefined)).toEqual(defaultHandoff());
    expect(sanitizeHandoff(null)).toEqual(defaultHandoff());
    expect(sanitizeHandoff("codex")).toEqual(defaultHandoff());
    expect(sanitizeHandoff({ provider: "gemini", kind: "orbital", permission: "yolo" })).toEqual(defaultHandoff());
  });

  it("restores a valid remembered handoff", () => {
    expect(sanitizeHandoff({ provider: "claude", kind: "external_terminal", permission: "edit" }))
      .toEqual({ provider: "claude", kind: "external_terminal", permission: "edit" });
    expect(sanitizeHandoff({ provider: "codex", kind: "managed", permission: "inspect" }))
      .toEqual({ provider: "codex", kind: "managed", permission: "inspect" });
  });

  it("never restores bypass, whatever was stored", () => {
    // Bypass turns off the agent's permission checks and its sandbox. A
    // preference must not be able to widen access on the next launch.
    expect(sanitizeHandoff({ provider: "codex", kind: "managed", permission: "bypass" }).permission).toBe("ask");
    expect(sanitizeHandoff({ permission: "bypass" }).permission).toBe("ask");
  });

  it("refuses a managed connection for a provider that has none", () => {
    // Grok has no managed adapter, so a stored `{grok, managed}` must come
    // back as a terminal handoff rather than a launch that cannot succeed.
    expect(sanitizeHandoff({ provider: "grok", kind: "managed", permission: "ask" }).kind)
      .toBe("external_terminal");
    expect(supportsManaged("grok")).toBe(false);
    expect(supportsManaged("agy")).toBe(false);
    // Both adapters that exist are restored as managed.
    expect(supportsManaged("codex")).toBe(true);
    expect(supportsManaged("claude")).toBe(true);
    expect(sanitizeHandoff({ provider: "claude", kind: "managed", permission: "ask" }).kind)
      .toBe("managed");
  });

  it("treats Grok as a first-class terminal-only provider", () => {
    expect(isAgentProvider("grok")).toBe(true);
    expect(supportsManaged("grok")).toBe(false);
    expect(sanitizeHandoff({ provider: "grok", kind: "managed", permission: "edit" }))
      .toEqual({ provider: "grok", kind: "external_terminal", permission: "edit" });
    expect(describeHandoff({ provider: "grok", kind: "external_terminal", permission: "ask" }))
      .toBe("Grok · terminal");
    expect(handoffGate({
      checkout: "/work/GitPulse",
      settings: { provider: "grok", kind: "managed", permission: "ask" },
      acknowledgedBypass: false,
      dirty: false,
      busy: false,
    }).reason).toMatch(/Grok supports terminal handoffs only/);
  });

  it("treats Antigravity as a first-class terminal-only provider", () => {
    expect(isAgentProvider("agy")).toBe(true);
    expect(supportsManaged("agy")).toBe(false);
    expect(sanitizeHandoff({ provider: "agy", kind: "managed", permission: "edit" }))
      .toEqual({ provider: "agy", kind: "external_terminal", permission: "edit" });
    expect(describeHandoff({ provider: "agy", kind: "external_terminal", permission: "ask" }))
      .toBe("Antigravity · terminal");
    expect(handoffGate({
      checkout: "/work/GitPulse",
      settings: { provider: "agy", kind: "managed", permission: "ask" },
      acknowledgedBypass: false,
      dirty: false,
      busy: false,
    }).reason).toMatch(/Antigravity supports terminal handoffs only/);
  });

  it("keeps a settings object coherent when the provider changes under it", () => {
    expect(reconcileHandoff({ provider: "grok", kind: "managed", permission: "ask" }))
      .toEqual({ provider: "grok", kind: "external_terminal", permission: "ask" });
    expect(reconcileHandoff({ provider: "codex", kind: "managed", permission: "ask" }).kind).toBe("managed");
    expect(reconcileHandoff({ provider: "claude", kind: "managed", permission: "ask" }).kind).toBe("managed");
  });

  it("describes a handoff in one readable phrase", () => {
    expect(describeHandoff({ provider: "claude", kind: "external_terminal", permission: "ask" }))
      .toBe("Claude Code · terminal");
    expect(describeHandoff({ provider: "codex", kind: "managed", permission: "ask" }))
      .toBe("Codex · managed");
  });
});

describe("normalizeCheckout", () => {
  it("accepts a plain path unchanged", () => {
    expect(normalizeCheckout("/work/GitPulse")).toBe("/work/GitPulse");
    expect(normalizeCheckout("  /work/GitPulse  ")).toBe("/work/GitPulse");
  });

  it("returns the original spelling rather than a normalized one", () => {
    // The normalizer rewrites backslashes for comparison; what is sent to the
    // backend has to stay the path the reader actually chose.
    expect(normalizeCheckout("C:\\work\\GitPulse")).toBe("C:\\work\\GitPulse");
  });

  it("refuses empty, oversized, control-laden and bare-separator input", () => {
    expect(normalizeCheckout("")).toBe("");
    expect(normalizeCheckout("   ")).toBe("");
    expect(normalizeCheckout("/work\u0000/evil")).toBe("");
    expect(normalizeCheckout("/work\u001b[2J")).toBe("");
    expect(normalizeCheckout("/")).toBe("");
    expect(normalizeCheckout("x".repeat(MAX_CHECKOUT_LENGTH + 1))).toBe("");
    expect(normalizeCheckout(42)).toBe("");
    expect(normalizeCheckout(null)).toBe("");
  });
});

describe("checkoutBesideCommonDir", () => {
  it("returns the working folder beside a .git directory", () => {
    expect(checkoutBesideCommonDir("/work/GitPulse/.git")).toBe("/work/GitPulse");
    expect(checkoutBesideCommonDir("C:\\work\\GitPulse\\.git")).toBe("C:\\work\\GitPulse");
  });

  it("returns nothing for a bare repository, which has no working tree", () => {
    expect(checkoutBesideCommonDir("/work/bare.git")).toBeNull();
    expect(checkoutBesideCommonDir("/.git")).toBeNull();
    expect(checkoutBesideCommonDir("")).toBeNull();
    expect(checkoutBesideCommonDir("/work/GitPulse")).toBeNull();
  });
});

describe("checkoutCandidates", () => {
  it("offers the open tabs for this repository first, labelled as open", () => {
    const candidates = checkoutCandidates(
      "r0",
      repositories,
      [
        { path: "/work/GitPulse", label: "GitPulse" },
        { path: "/work/Manvi", label: "Manvi" },
      ],
      options,
    );
    expect(candidates).toEqual([{ path: "/work/GitPulse", label: "GitPulse", source: "open" }]);
    expect(preferredCheckout(candidates)).toBe("/work/GitPulse");
  });

  it("falls back to the folder beside the common dir, marked as derived", () => {
    const candidates = checkoutCandidates("r0", repositories, [], options);
    expect(candidates).toEqual([{ path: "/work/GitPulse", label: "/work/GitPulse", source: "derived" }]);
  });

  it("never lists the same checkout twice", () => {
    const candidates = checkoutCandidates(
      "r0",
      repositories,
      [
        { path: "/work/GitPulse", label: "GitPulse" },
        { path: "/work/GitPulse/", label: "GitPulse again" },
      ],
      options,
    );
    expect(candidates).toHaveLength(1);
    expect(candidates[0].source).toBe("open");
  });

  it("offers nothing for a bare repository or an unknown identity", () => {
    expect(checkoutCandidates("r1", repositories, [], options)).toEqual([]);
    expect(checkoutCandidates("r2", repositories, [], options)).toEqual([]);
    expect(checkoutCandidates("missing", repositories, [], options)).toEqual([]);
    expect(preferredCheckout([])).toBe("");
  });

  it("matches a tab opened at the .git directory itself", () => {
    const candidates = checkoutCandidates("r0", repositories, [{ path: "/work/GitPulse/.git", label: "dotgit" }], options);
    expect(candidates[0]).toMatchObject({ path: "/work/GitPulse/.git", source: "open" });
  });

  it("honours case-insensitive filesystems without duplicating a checkout", () => {
    const candidates = checkoutCandidates(
      "r0",
      repositories,
      [
        { path: "/work/GitPulse", label: "GitPulse" },
        { path: "/WORK/gitpulse", label: "shouted" },
      ],
      { caseInsensitive: true },
    );
    expect(candidates).toHaveLength(1);
  });

  it("skips a tab whose path is not usable at all", () => {
    const candidates = checkoutCandidates("r0", repositories, [{ path: "/work\u0000/GitPulse", label: "bad" }], options);
    expect(candidates.every((entry) => entry.source === "derived")).toBe(true);
  });

  it("stays bounded with many open tabs", () => {
    const tabs = Array.from({ length: 5_000 }, (_, i) => ({ path: `/work/other-${i}`, label: `o${i}` }));
    const started = performance.now();
    const candidates = checkoutCandidates("r0", repositories, tabs, options);
    // Catches an accidental O(n^2) over the tab list, which at 5,000 tabs is
    // 25M comparisons and lands in seconds; the bound is sized to separate
    // that from linear work on a machine that is also running a build.
    expect(performance.now() - started).toBeLessThan(2_000);
    expect(candidates).toHaveLength(1);
  });
});

describe("handoffGate", () => {
  const base = {
    checkout: "/work/GitPulse",
    settings: defaultHandoff(),
    acknowledgedBypass: false,
    dirty: false,
    busy: false,
  };

  it("passes a complete, saved, idle handoff", () => {
    expect(handoffGate(base)).toEqual({ ok: true, reason: "" });
  });

  it("names the one thing to fix, in the order a reader would fix it", () => {
    expect(handoffGate({ ...base, busy: true }).reason).toMatch(/already in progress/);
    expect(handoffGate({ ...base, dirty: true }).reason).toMatch(/Save your task edits/);
    expect(handoffGate({ ...base, checkout: "" }).reason).toMatch(/working checkout/);
    expect(handoffGate({ ...base, checkout: "  " }).reason).toMatch(/working checkout/);
  });

  it("reports a busy launch ahead of an unsaved edit", () => {
    // Both are true; the one that will clear on its own is named first.
    expect(handoffGate({ ...base, busy: true, dirty: true }).reason).toMatch(/already in progress/);
  });

  it("refuses a managed connection the provider does not offer", () => {
    const gate = handoffGate({ ...base, settings: { provider: "grok", kind: "managed", permission: "ask" } });
    expect(gate.ok).toBe(false);
    expect(gate.reason).toMatch(/Grok supports terminal handoffs only/);
    // And lets through the two that do offer one.
    for (const provider of ["codex", "claude"] as const) {
      expect(handoffGate({ ...base, settings: { provider, kind: "managed", permission: "ask" } }).ok).toBe(true);
    }
  });

  it("requires an explicit authorization for every bypass attempt", () => {
    const bypass = { ...base, settings: { ...base.settings, permission: "bypass" as const } };
    expect(handoffGate(bypass).ok).toBe(false);
    expect(handoffGate(bypass).reason).toMatch(/explicit authorization/);
    expect(handoffGate({ ...bypass, acknowledgedBypass: true }).ok).toBe(true);
  });
});

describe("runStateLabel", () => {
  it("names every state the wire declares, and passes an unknown one through", () => {
    expect(runStateLabel("prepared")).toBe("Prepared");
    expect(runStateLabel("exited")).toBe("Exited");
    expect(runStateLabel("failed")).toBe("Failed to start");
    expect(runStateLabel("unresolved")).toBe("Unresolved");
    // An unknown state from a newer backend is shown, not swallowed.
    expect(runStateLabel("quantum")).toBe("quantum");
  });
});
