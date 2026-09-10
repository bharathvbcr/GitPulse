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
