import { describe, expect, it } from "vitest";
import {
  MAX_APPLE_INPUT_CHARS,
  MAX_CONTEXT_LABELS,
  MAX_CONTEXT_REPOSITORIES,
  appleBadge,
  appleContext,
  appleErrorCode,
  appleGate,
  appleReady,
  explainAppleError,
  isAppleStatus,
  type AppleIntelligenceStatus,
} from "./appleIntelligence";

const available: AppleIntelligenceStatus = {
  compiled: true,
  state: "available",
  reason: null,
  detail: "The on-device model is ready. Nothing leaves this Mac.",
};

const request = {
  fields: ["title", "description"],
  notes: "the board menu cannot set a due date",
  title: "",
  description: "",
  context: "Repository: GitPulse",
};

describe("isAppleStatus", () => {
  it("accepts only the shape the command actually returns", () => {
    expect(isAppleStatus(available)).toBe(true);
    expect(isAppleStatus({ ...available, reason: "model_not_ready" })).toBe(true);
    for (const bad of [
      null,
      undefined,
      "available",
      { ...available, state: "ready" },
      { ...available, compiled: "true" },
      { ...available, reason: 7 },
      { ...available, detail: null },
      {},
    ]) {
      expect(isAppleStatus(bad), JSON.stringify(bad)).toBe(false);
    }
  });
});

describe("appleBadge", () => {
  it("keeps the three kinds of no apart", () => {
    // A reader can act on two of these and not on the third. One shared word
    // would send someone to System Settings to fix their build.
    expect(appleBadge(available)).toBe("On this Mac");
    expect(appleBadge({ compiled: false, state: "unsupported_os", reason: "not_compiled", detail: "x" })).toBe("Not in this build");
    expect(appleBadge({ compiled: true, state: "unavailable", reason: "apple_intelligence_not_enabled", detail: "x" })).toBe("Turned off");
    expect(appleBadge({ compiled: true, state: "unavailable", reason: "model_not_ready", detail: "x" })).toBe("Preparing");
    expect(appleBadge({ compiled: true, state: "unavailable", reason: "device_not_eligible", detail: "x" })).toBe("Unsupported Mac");
    // A reason this version has never heard of still gets an honest word.
    expect(appleBadge({ compiled: true, state: "unavailable", reason: "invented_later", detail: "x" })).toBe("Unavailable");
    expect(appleBadge(null)).toBe("");
  });

  it("treats a missing status as not ready", () => {
    expect(appleReady(null)).toBe(false);
    expect(appleReady({ compiled: true, state: "unavailable", reason: null, detail: "" })).toBe(false);
    expect(appleReady(available)).toBe(true);
  });
});

describe("appleContext", () => {
  it("carries only facts the task already holds", () => {
    expect(appleContext({ kind: "bug", repositories: ["GitPulse", "Manvi"], labels: ["ci", "flake"] }))
      .toBe("Repository: GitPulse, Manvi. Task type: bug. Labels: ci, flake");
    expect(appleContext({})).toBe("");
    expect(appleContext({ repositories: ["", "  "], labels: [" "] })).toBe("");
  });

  it("bounds what it will hand to the model", () => {
    const many = Array.from({ length: 40 }, (_, i) => `repo-${i}`);
    const labels = Array.from({ length: 40 }, (_, i) => `label-${i}`);
    const context = appleContext({ repositories: many, labels });
    expect(context).toContain("repo-7");
    expect(context).not.toContain("repo-8");
    expect(context).toContain("label-11");
    expect(context).not.toContain("label-12");
  });

  it("says how many it withheld, so a bounded list does not read as the whole set", () => {
    // Eight of forty presented as a closed list is how the model comes to
    // write "affects both repositories" about a task that spans five more.
    const context = appleContext({
      repositories: Array.from({ length: 40 }, (_, i) => `repo-${i}`),
      labels: Array.from({ length: 40 }, (_, i) => `label-${i}`),
    });
    expect(context).toContain(`(and ${40 - MAX_CONTEXT_REPOSITORIES} more not listed)`);
    expect(context).toContain(`(and ${40 - MAX_CONTEXT_LABELS} more not listed)`);
    // A list that fits is stated plainly; a marker there would be a lie.
    expect(appleContext({ repositories: ["GitPulse", "Manvi"], labels: ["ci"] }))
      .toBe("Repository: GitPulse, Manvi. Labels: ci");
    // The cap counts what survives filtering, not what was passed in.
    const padded = [...Array.from({ length: MAX_CONTEXT_REPOSITORIES }, (_, i) => `r${i}`), "  ", ""];
    expect(appleContext({ repositories: padded })).not.toContain("not listed");
  });
});

describe("appleGate", () => {
  it("will not pretend to know before the status arrives", () => {
    expect(appleGate(null, request)).toEqual({ ok: false, reason: "Checking Apple Intelligence…" });
  });

  it("repeats the framework's own reason rather than inventing one", () => {
    const off: AppleIntelligenceStatus = {
      compiled: true,
      state: "unavailable",
      reason: "apple_intelligence_not_enabled",
      detail: "Turn on Apple Intelligence in System Settings to use it here.",
    };
    expect(appleGate(off, request)).toEqual({ ok: false, reason: off.detail });
  });

  it("refuses an empty ask and an empty task", () => {
    expect(appleGate(available, { ...request, fields: [] }).reason).toBe("Choose a field to write.");
    expect(appleGate(available, { ...request, notes: "   " }).reason).toBe("Write some notes first.");
    // A title alone is enough to improve; the model is not given the repo.
    expect(appleGate(available, { ...request, notes: "", title: "Fix the menu" }).ok).toBe(true);
  });

  it("counts characters, not bytes, exactly as the Rust side does", () => {
    const astral = "𝄞".repeat(MAX_APPLE_INPUT_CHARS - 1);
    expect(appleGate(available, { ...request, notes: astral, context: "" }).ok).toBe(true);
    expect(appleGate(available, { ...request, notes: astral + "𝄞𝄞", context: "" }).ok).toBe(false);
    expect(appleGate(available, { ...request, notes: astral + "𝄞𝄞", context: "" }).reason).toContain("12,000");
  });

  it("passes a task that fits", () => {
    expect(appleGate(available, request)).toEqual({ ok: true, reason: "" });
  });
});

describe("explainAppleError", () => {
  it("keeps the command's coded refusal intact", () => {
    const refusal = { code: "refused", message: "Apple Intelligence declined to write this." };
    expect(explainAppleError(refusal)).toBe(refusal.message);
    expect(appleErrorCode(refusal)).toBe("refused");
  });

  it("still says something useful for anything else", () => {
    expect(explainAppleError(new Error("boom"))).toBe("boom");
    expect(explainAppleError(null)).toBe("Apple Intelligence could not finish.");
    expect(explainAppleError({ message: "   " })).toBe("Apple Intelligence could not finish.");
    expect(appleErrorCode(new Error("boom"))).toBe("worker_error");
    expect(appleErrorCode({ code: "" })).toBe("worker_error");
  });
});
