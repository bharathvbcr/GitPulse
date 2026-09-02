import { describe, expect, it } from "vitest";
import {
  contextWindowLabel,
  sweepSummary,
  toolSupportLabel,
  type ScanModel,
  type ScanResult,
} from "./scan";

function model(overrides: Partial<ScanModel> = {}): ScanModel {
  return {
    id: "m",
    context_window: 8192,
    context_window_source: "declared",
    implausible_window: 0,
    capabilities_known: true,
    supports_tools: true,
    supports_reasoning: false,
    supports_vision: false,
    supports_completion: true,
    ...overrides,
  };
}

describe("toolSupportLabel", () => {
  it("distinguishes 'no tools' from 'nobody asked'", () => {
    // The distinction this flag exists for. Rendering both as "no tools" makes
    // a capable model look incapable; rendering both as "tools" offers a
    // feature that will fail and look like the user's configuration.
    expect(toolSupportLabel(model({ supports_tools: true }))).toBe("tools");
    expect(toolSupportLabel(model({ supports_tools: false }))).toBe("no tools");
    expect(toolSupportLabel(model({ capabilities_known: false, supports_tools: false }))).toBe(
      "tool support unknown",
    );
    expect(toolSupportLabel(model({ capabilities_known: false, supports_tools: true }))).toBe(
      "tool support unknown",
    );
  });
});

describe("contextWindowLabel", () => {
  it("reports a window when there is one", () => {
    expect(contextWindowLabel(model({ context_window: 131072 }))).toContain("131,072");
  });

  it("says a missing window is unreported, not zero", () => {
    // A model shown with a window of 0 reads as broken. It is not.
    expect(contextWindowLabel(model({ context_window: 0 }))).toBe("window unreported");
  });

  it("surfaces a window the scanner refused", () => {
    // The refusal is reportable rather than silent: an operator seeing
    // "unreported" would go looking for a server misconfiguration that is
    // actually a value the harness deliberately rejected.
    const label = contextWindowLabel(model({ context_window: 0, implausible_window: 999999999 }));
    expect(label).toContain("refused");
    expect(label).toContain("999,999,999");
  });
});

describe("sweepSummary", () => {
  it("says how hard it looked when it found nothing", () => {
    // "Nothing is running" and "we only looked in one place" are different.
    const empty: ScanResult = { servers: [], scanned: 5, capabilities: true };
    expect(sweepSummary(empty)).toContain("5 endpoints");
  });

  it("counts servers and models", () => {
    const result: ScanResult = {
      servers: [
        { base_url: "u", runtime: "ollama", version: "", models: [model(), model({ id: "n" })] },
      ],
      scanned: 5,
      capabilities: true,
    };
    expect(sweepSummary(result)).toContain("1 server");
    expect(sweepSummary(result)).toContain("2 models");
  });

  it("flags a sweep that never asked for capabilities", () => {
    const result: ScanResult = {
      servers: [{ base_url: "u", runtime: "ollama", version: "", models: [model()] }],
      scanned: 1,
      capabilities: false,
    };
    expect(sweepSummary(result)).toContain("capabilities not queried");
  });
});
