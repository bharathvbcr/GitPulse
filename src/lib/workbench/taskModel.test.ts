import { describe, expect, it } from "vitest";
import {
  describeSelection,
  effectiveSelection,
  explainEnhancementFailure,
  normalizeSelection,
  selectionWire,
} from "./taskModel";

const ok = { base_url: "http://127.0.0.1:11434/v1", model: "qwen3.8:27b-mlx" };

describe("effectiveSelection", () => {
  it("prefers preferred over ai.selected", () => {
    expect(effectiveSelection({
      preferred: ok,
      ai: { selected: { base_url: "http://127.0.0.1:1234/v1", model: "other" } },
    })).toEqual(ok);
  });

  it("falls back to ai.selected when preferred is null", () => {
    expect(effectiveSelection({ preferred: null, ai: { selected: ok } })).toEqual(ok);
  });

  it("returns null for empty scan, null ai, or both missing", () => {
    expect(effectiveSelection({ preferred: null, ai: null })).toBeNull();
    expect(effectiveSelection({ preferred: null, ai: { selected: null } })).toBeNull();
    expect(effectiveSelection({ preferred: { base_url: "", model: "" }, ai: { selected: null } })).toBeNull();
  });
});

describe("normalizeSelection", () => {
  it("refuses oversize ids, control chars, and non-loopback base_url", () => {
    expect(normalizeSelection({ base_url: "http://127.0.0.1:11434/v1", model: "x".repeat(129) })).toBeNull();
    expect(normalizeSelection({ base_url: `http://127.0.0.1:11434/${"a".repeat(500)}`, model: "m" })).toBeNull();
    expect(normalizeSelection({ base_url: "http://127.0.0.1:11434/v1", model: "bad\nmodel" })).toBeNull();
    expect(normalizeSelection({ base_url: "http://example.com:11434/v1", model: "m" })).toBeNull();
    expect(normalizeSelection({ base_url: "http://10.0.0.1:11434/v1", model: "m" })).toBeNull();
    expect(normalizeSelection({ base_url: "http://localhost:11434/v1", model: "m" })).toEqual({
      base_url: "http://localhost:11434/v1",
      model: "m",
    });
  });
});

describe("describeSelection", () => {
  it("formats model and host:port", () => {
    expect(describeSelection(ok)).toBe("qwen3.8:27b-mlx on 127.0.0.1:11434");
    expect(describeSelection({ base_url: "http://localhost:1234/v1", model: "gemma" })).toBe("gemma on localhost:1234");
  });
});

describe("selectionWire", () => {
  it("omits model when selection is absent or invalid", () => {
    expect(selectionWire(null)).toEqual({});
    expect(selectionWire({ base_url: "http://evil.test/v1", model: "x" })).toEqual({});
    expect(selectionWire(ok)).toEqual({ model: ok });
  });
});

describe("Claude selections", () => {
  const claude = { provider: "anthropic" as const, model: "claude-sonnet-5-5" };

  it("wins over a local selection when the Manvi pane chose Claude", () => {
    expect(effectiveSelection({ preferred: ok, ai: { selected: ok }, manviClaude: { model: "claude-sonnet-5-5" } })).toEqual(claude);
    expect(effectiveSelection({ preferred: ok, ai: null, manviClaude: null })).toEqual(ok);
  });

  it("never falls back to a local model when the Claude choice is invalid", () => {
    expect(effectiveSelection({ preferred: ok, ai: null, manviClaude: { model: "gpt-5" } })).toBeNull();
  });

  it("carries the provider on the wire and never a base_url", () => {
    expect(selectionWire(claude)).toEqual({ model: claude });
    expect(selectionWire({ provider: "anthropic", model: " claude-opus-5-5 " })).toEqual({
      model: { provider: "anthropic", model: "claude-opus-5-5" },
    });
    for (const model of ["", "gpt-5", "claude-x\ny","claude- spaced", `claude-${"a".repeat(130)}`]) {
      expect(selectionWire({ provider: "anthropic", model })).toEqual({});
    }
  });

  it("describes itself as the Claude API", () => {
    expect(describeSelection(claude)).toBe("claude-sonnet-5-5 via the Claude API");
  });

  it("passes key refusals through as written, never as a retry", () => {
    // Each names its own remedy; none may be mistaken for "still starting".
    for (const text of [
      "Manvi is set to use Claude, but no Claude API key is saved. Add one in the Manvi pane, or switch Manvi back to a local model.",
      "`/usr/local/bin/manvi` does not promise to keep ANTHROPIC_API_KEY from the programs it starts, so GitPulse will not give it your Claude API key. Update Manvi.",
      "the running harness does not promise to keep ANTHROPIC_API_KEY from the programs it starts; stopped it rather than leave your Claude API key with it",
    ]) {
      expect(explainEnhancementFailure(text)).toEqual({ guidance: text, action: null });
    }
  });
});

describe("explainEnhancementFailure", () => {
  it.each([
    ["model not served by endpoint", "not served", "change_model"],
    ["ErrNotServed", "not served", "change_model"],
    ["StopMaxTokens applied", "output limit", "retry"],
    ["generation cancelled", "Cancelled: the model selection changed", "restart"],
    ["lease expired", "expired", "restart"],
    ["worker busy", "busy", "wait"],
    ["transport timeout", "still starting", "retry"],
    ["unavailable", "still starting", "retry"],
  ] as const)("maps %s", (text, expectGuidance, action) => {
    const advice = explainEnhancementFailure(text);
    expect(advice.guidance.toLowerCase()).toContain(expectGuidance.toLowerCase());
    expect(advice.action).toBe(action);
  });

  it("passes through unknown failures", () => {
    expect(explainEnhancementFailure("weird decoder fault")).toEqual({
      guidance: "weird decoder fault",
      action: null,
    });
  });
});
